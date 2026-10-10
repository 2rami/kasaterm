//! claude 계정 슬롯 — 키체인·자격 파일 읽기/쓰기, OAuth 토큰 새로 고침, 금고(vault) 판정,
//! `/claude-identity`·`/claude-usage` 와 사용량 캐시.

use super::*;
#[cfg(test)]
use super::test_support::temp_dir;

/// Keychain service name holding one account's credentials.
///
/// Claude Code (2.1.220, function `oG`) appends `-<sha256(store path)[0..8]>`
/// whenever the credential store is overridden, and nothing when it is not. We
/// mirror that instead of tracking items ourselves, so the usage pill reads the
/// account the panes are actually running as. `dir` must be the same string the
/// shim exports — the CLI NFC-normalises it before hashing, and a path that is
/// already NFC (everything we generate) hashes identically.
pub(crate) fn claude_keychain_service(dir: Option<&str>) -> String {
    const BASE: &str = "Claude Code-credentials";
    match dir.filter(|d| !d.is_empty()) {
        None => BASE.to_string(),
        Some(d) => {
            use sha2::{Digest, Sha256};
            let h = Sha256::digest(d.as_bytes());
            format!("{BASE}-{}", &format!("{h:x}")[..8])
        }
    }
}

/// claude oauth API 토큰 — 주어진 계정 저장소에서 읽는다. macOS 는 Keychain,
/// 그 외는 저장소 dir 의 `.credentials.json`. 빈 값/None = 기본 로그인.
///
/// 활성 계정 경로는 `KASATERM_CLAUDE_ACCOUNT_DIR`(kasaterm 이 shim 을 깔 때마다
/// 자기 프로세스 env 에 갱신)에서 오지만, env 를 읽는 것은 **호출자**다 — 그래야
/// 캐시 키와 조회 대상이 같은 값에서 나오고, 테스트가 서로 env 를 안 밟는다.
fn read_claude_token_from(account_dir: Option<&str>) -> Option<String> {
    let (v, _) = read_claude_credentials(account_dir)?;
    // claude 는 갱신이 `invalid_grant` 로 거부되면 그 자리에 빈 토큰을 써 둔다. 빈 값을
    // 토큰으로 넘기면 프로필 조회가 「일시 실패」로 읽혀 로그아웃 슬롯이 로그인돼 보였다.
    v.pointer("/claudeAiOauth/accessToken")
        .and_then(|t| t.as_str())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

/// 슬롯 로그인 상태. `claude auth status` 와 같은 판정을 claude 를 띄우지 않고 한다 —
/// 띄우면 만료 토큰을 그 자리에서 회전시켜 다른 갱신과 부딪힌다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotLogin {
    LoggedIn,
    LoggedOut,
    /// 저장소를 못 열었다(ssh 세션의 잠긴 키체인 등). 로그아웃으로 단정하지 않는다.
    Unknown,
}

/// 그 슬롯에 쓸 수 있는 로그인이 있나. 값은 꺼내지 않고 있음/없음만 본다.
///
/// 로그인 = 갱신 토큰이 있거나 아직 안 끝난 access token 이 있음. 단 이 프로세스가
/// 그 갱신 토큰을 서버에서 거부당했고(`dead_refresh`) access token 도 끝났으면 로그아웃이다.
pub fn claude_slot_login(account_dir: Option<&str>) -> SlotLogin {
    let account_dir = account_dir.filter(|s| !s.is_empty());
    let Some(creds_dir) = account_dir
        .map(std::path::PathBuf::from)
        .or_else(|| Some(kasa_socket::home_dir()?.join(".claude")))
    else {
        return SlotLogin::Unknown;
    };
    let mut docs: Vec<serde_json::Value> = std::fs::read_to_string(creds_dir.join(".credentials.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .into_iter()
        .collect();
    let mut unknown = false;
    if cfg!(target_os = "macos") {
        let svc = claude_keychain_service(account_dir);
        let read = |args: &[&str]| crate::no_window_command("security").args(args).output().ok();
        let out = keychain_user()
            .and_then(|u| read(&["find-generic-password", "-s", &svc, "-a", &u, "-w"]))
            .filter(|o| o.status.success())
            .or_else(|| read(&["find-generic-password", "-s", &svc, "-w"]));
        match out {
            Some(o) if o.status.success() => {
                if let Ok(v) = serde_json::from_slice::<serde_json::Value>(o.stdout.trim_ascii()) {
                    docs.push(v);
                }
            }
            // 44 = errSecItemNotFound. 그 밖(36 잠김 등)은 있는지조차 모른다.
            Some(o) if o.status.code() == Some(44) => {}
            _ => unknown = true,
        }
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let rejected = account_dir.is_some_and(|d| dead_refresh().lock().is_ok_and(|g| g.contains(d)));
    if docs.iter().any(|v| oauth_usable(v, now_ms, rejected)) {
        SlotLogin::LoggedIn
    } else if unknown {
        SlotLogin::Unknown
    } else {
        SlotLogin::LoggedOut
    }
}

fn oauth_usable(doc: &serde_json::Value, now_ms: u64, refresh_rejected: bool) -> bool {
    let Some(o) = doc.get("claudeAiOauth") else { return false };
    let has = |k: &str| o.get(k).and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty());
    let live_access = has("accessToken")
        && o.get("expiresAt").and_then(|v| v.as_u64()).is_some_and(|e| e > now_ms);
    live_access || (has("refreshToken") && !refresh_rejected)
}

/// 자격증명이 **어디서 왔는지**. 갱신한 값은 읽은 자리에 그대로 되써야 한다 —
/// 파일에서 읽고 키체인에 쓰면 claude CLI 는 옛 값을 계속 보고, 그 반대면 우리가
/// 회전시킨 refresh token 을 CLI 가 모른 채 옛것으로 갱신을 시도해 죽는다.
enum CredSource {
    File(std::path::PathBuf),
    Keychain(String),
}

/// 슬롯의 자격증명 문서 전체 + 그 출처. macOS 는 Keychain, 그 외는 저장소 dir 의
/// `.credentials.json`. 빈 값/None = 기본 로그인.
///
/// **둘 다 있으면 만료가 늦은 쪽을 쓴다.** 예전엔 파일을 먼저 찾고 있으면 거기서
/// 끝냈는데, macOS 에서 claude CLI 가 갱신하는 정본은 키체인이라 한 번 남은
/// `~/.claude/.credentials.json` 은 아무도 안 고쳐 주고 몇 시간이면 썩는다. 그러면
/// 살아 있는 키체인 토큰을 눈앞에 두고 죽은 파일 토큰으로 401 을 받아, 화면은
/// 기본 계정을 영영 "확인 중…" 으로 붙잡는다(사용자 2026-08-13. 실측: 파일 토큰은
/// 11:09 만료·401, 같은 시각 키체인 토큰은 200 이었다).
fn read_claude_credentials(account_dir: Option<&str>) -> Option<(serde_json::Value, CredSource)> {
    let account_dir = account_dir.filter(|s| !s.is_empty());
    let creds_dir = match account_dir {
        Some(d) => d.to_string(),
        None => format!("{}/.claude", kasa_socket::home_dir()?.display()),
    };
    let file = std::path::PathBuf::from(&creds_dir).join(".credentials.json");
    let from_file = std::fs::read_to_string(&file)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .map(|v| (v, CredSource::File(file)));
    let svc = claude_keychain_service(account_dir);
    // 계정 칸(`-a`)까지 맞춰 읽는다. 같은 서비스명의 항목이 둘일 수 있다 — 2026-09-03
    // 실측: 기본 항목 옆에 account="unknown" 인 빈 껍데기(08-18 사고 잔재)가 있어
    // 이름만으로 읽으면 그 껍데기가 먼저 잡혔고, 활성 계정 신원이 「빈손」이 됐다.
    // claude 는 로그인 사용자명을 계정 칸에 쓴다(`K7()`). 그 이름으로 못 찾을 때만
    // 이름만으로 한 번 더 본다.
    let read_secret = |args: &[&str]| {
        crate::no_window_command("security")
            .args(args)
            .output()
            .ok()
            .and_then(|out| String::from_utf8(out.stdout).ok())
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s.trim()).ok())
    };
    let from_keychain = keychain_user()
        .and_then(|u| read_secret(&["find-generic-password", "-s", &svc, "-a", &u, "-w"]))
        .or_else(|| read_secret(&["find-generic-password", "-s", &svc, "-w"]))
        .map(|v| (v, CredSource::Keychain(svc)));
    let expires_at = |v: &serde_json::Value| {
        v.pointer("/claudeAiOauth/expiresAt")
            .and_then(|e| e.as_u64())
            .unwrap_or(0)
    };
    match (from_file, from_keychain) {
        (Some(f), Some(k)) => Some(if expires_at(&k.0) > expires_at(&f.0) { k } else { f }),
        (some, None) | (None, some) => some,
    }
}

/// 갱신한 자격증명을 읽은 자리에 되쓴다.
///
/// ⚠️ 키체인 경로는 토큰이 `security` 의 **argv 에 실린다**. `security(1)` 은 비밀을
/// stdin 으로 받는 길이 없고, 대신 Security 프레임워크를 직접 부르면 우리 프로세스가
/// 남이 만든 키체인 항목을 건드리는 꼴이라 macOS 가 접근 승인 창을 띄운다. 읽기가
/// 이미 같은 도구를 거치고 있어 권한 모델을 안 흔드는 쪽을 골랐다.
fn write_claude_credentials(src: &CredSource, v: &serde_json::Value) -> bool {
    let Ok(body) = serde_json::to_string(v) else {
        return false;
    };
    match src {
        CredSource::File(p) => std::fs::write(p, body).is_ok(),
        CredSource::Keychain(svc) => {
            // 계정(-a)이 다르면 같은 서비스에 **항목이 하나 더 생긴다** — 그러면
            // claude 가 어느 쪽을 볼지 알 수 없으니 기존 항목의 acct 를 그대로 쓴다.
            let acct = keychain_account(svc).unwrap_or_default();
            if acct.is_empty() {
                return false;
            }
            crate::no_window_command("security")
                .args([
                    "add-generic-password", "-U", "-a", &acct, "-s", svc, "-w", &body,
                ])
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        }
    }
}

/// claude 가 키체인 항목의 계정 칸에 쓰는 로그인 사용자명. env 가 비면 홈 폴더
/// 이름으로 — Finder 로 띄운 앱은 USER 가 없을 수 있다.
pub(crate) fn keychain_user() -> Option<String> {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            kasa_socket::home_dir()?
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
        })
}

/// 그 키체인 항목의 `acct` 필드. `security find-generic-password` 는 값을 뺀 속성
/// 덤프를 stdout 으로 준다(`"acct"<blob>="kasa"`).
fn keychain_account(svc: &str) -> Option<String> {
    let out = crate::no_window_command("security")
        .args(["find-generic-password", "-s", svc])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.trim().strip_prefix("\"acct\"<blob>=\""))
        .and_then(|r| r.strip_suffix('"'))
        .map(str::to_string)
}

/// claude CLI 가 쓰는 공개 OAuth 클라이언트. 토큰 엔드포인트도 CLI 와 같은 것이라,
/// 여기서 회전시킨 토큰을 CLI 가 그대로 이어 쓴다.
const CLAUDE_OAUTH_TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";

const CLAUDE_OAUTH_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";

/// 그 슬롯으로 claude 가 지금 돌고 있나.
///
/// 모르면 **있다고 답한다** — 판정이 한쪽으로만 틀리게 골랐다. 없는데 있다고 하면
/// 갱신을 한 번 거를 뿐이지만, 있는데 없다고 하면 도는 세션의 토큰을 빼앗는다.
fn slot_has_live_claude(dir: &str) -> bool {
    // 기본 슬롯은 env 없이 도는 모든 claude 가 쓴다 — 셀 방법이 없으니 늘 산 것으로.
    if dir.is_empty() {
        return true;
    }
    let Ok(out) = crate::no_window_command("ps")
        .args(["eww", "-ax", "-o", "command="])
        .output()
    else {
        return true;
    };
    String::from_utf8_lossy(&out.stdout)
        .contains(&format!("CLAUDE_SECURESTORAGE_CONFIG_DIR={dir}"))
}

/// 만료된(또는 5분 안에 만료될) access token 을 refresh token 으로 되살린다.
/// 갱신했으면 새 access token, 갱신할 필요/방법이 없으면 None.
///
/// **왜 우리가 하나**: 토큰 갱신은 그 계정으로 `claude` 가 실제로 돌 때만 일어난다.
/// 그래서 안 쓰는 슬롯일수록 더 깜깜해지고, 정작 "어디로 옮길까" 고르려고 여는
/// 계정 목록이 **옮기기 전엔 아무것도 못 알려주는** 닭-달걀이 된다(2026-08-11 실측:
/// 두 슬롯이 각각 10시간·79시간 전 만료라 사용량도 신원도 전부 빈칸이었다).
///
/// ⚠️ refresh token 은 **1회용**이다. 도는 CLI 도 같은 토큰을 회전시키려 하므로,
/// 먼저 쓴 쪽만 살고 나머지는 `invalid_grant` 로 죽는다 — 그 슬롯 세션이 통째로
/// 로그아웃된다. 그래서 살아 있는 슬롯은 건드리지 않는다(어차피 CLI 가 갱신해 준다).
async fn refresh_claude_token(dir: &str) -> Option<String> {
    // ⚠️ 활성 계정의 금고 토큰은 여기서 회전시키지 않는다 — 이 함수는 refresh
    // token 을 **직접 소비**하므로(OAuth POST) 활성 금고에 돌면 작업대의 사슬이
    // 그 자리에서 죽는다. slot_has_live_claude 는 env 문자열만 봐서 env 없이
    // 작업대를 쓰는 활성 pane 들을 못 보고, 그래서 활성 금고를 늘 「안 쓰는
    // 슬롯」으로 판정한다 — 그 게이트만으론 못 막는다(2026-08-19 조사 확정).
    if managed_vault_refresh_forbidden(dir) {
        eprintln!("[claude-token] 활성 계정 금고 회전 거부 — 작업대가 정본이다");
        return None;
    }
    // 사용량·신원 창구가 같은 슬롯을 동시에 물으면 둘 다 같은 갱신 토큰으로 회전을
    // 시도했다. 진 쪽은 400 이고, 서버가 재사용을 감지하면 이긴 쪽의 새 토큰까지 무효가
    // 돼 슬롯이 통째로 죽는다(2026-09-24 맥북: 같은 주기에 두 슬롯이 함께 끊겼다).
    // 슬롯마다 한 줄로 세우고, 기다린 쪽은 앞사람이 저장한 새 토큰을 다시 읽는다.
    let gate = refresh_gate(dir);
    let _turn = gate.lock().await;
    let (mut creds, src) = read_claude_credentials(Some(dir))?;
    let oauth = creds.get("claudeAiOauth")?.as_object()?;
    let expires_at = oauth.get("expiresAt")?.as_u64()?;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis() as u64;
    // CLI 와 같은 5분 스큐 — 조회 도중 만료되는 걸 피한다.
    if expires_at > now_ms + 5 * 60 * 1000 {
        return None;
    }
    if slot_has_live_claude(dir) {
        return None;
    }
    let refresh = oauth.get("refreshToken")?.as_str().filter(|r| !r.is_empty())?.to_string();
    let resp = reqwest::Client::new()
        .post(CLAUDE_OAUTH_TOKEN_URL)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh.as_str()),
            ("client_id", CLAUDE_OAUTH_CLIENT_ID),
        ])
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        // 상태만 남긴다(토큰은 절대). 400/401=죽은 refresh token, 429=스로틀 —
        // 조용한 None 은 성공과 구분이 안 돼 현장에서 진단이 불가능하다.
        let code = resp.status().as_u16();
        eprintln!("[claude-token] 갱신 거부 {code} · slot={dir}");
        if let (400 | 401 | 403, Ok(mut g)) = (code, dead_refresh().lock()) {
            g.insert(dir.to_string());
        }
        return None;
    }
    // `.json()` 은 reqwest 의 json feature 가 필요한데 이 크레이트는 안 켰다 —
    // 본문을 받아 직접 파싱한다(의존성 하나를 아끼려고).
    if let Ok(mut g) = dead_refresh().lock() {
        g.remove(dir);
    }
    let data: serde_json::Value = serde_json::from_str(&resp.text().await.ok()?).ok()?;
    let access = data.get("access_token")?.as_str()?.to_string();
    let o = creds.get_mut("claudeAiOauth")?.as_object_mut()?;
    o.insert("accessToken".into(), access.clone().into());
    if let Some(exp) = data.get("expires_in").and_then(|v| v.as_u64()) {
        o.insert("expiresAt".into(), (now_ms + exp * 1000).into());
    }
    // **회전된 refresh token 을 반드시 남긴다.** 이걸 빠뜨리면 다음 갱신이 죽은
    // 토큰으로 나가 그 슬롯이 로그아웃된다 — 되살리려던 기능이 계정을 깨는 길.
    if let Some(r) = data.get("refresh_token").and_then(|v| v.as_str()) {
        o.insert("refreshToken".into(), r.into());
    }
    if !write_claude_credentials(&src, &creds) {
        eprintln!("[claude-token] 갱신은 됐는데 저장 실패 · slot={dir} — 이 슬롯은 재로그인이 필요할 수 있다");
        return None;
    }
    Some(access)
}

fn refresh_gate(dir: &str) -> std::sync::Arc<tokio::sync::Mutex<()>> {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};
    static GATES: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    let mut gates = GATES.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    gates.entry(dir.to_string()).or_default().clone()
}

/// `GET /claude-usage` — claude oauth usage API(5시간/주간 한도·사용률·리셋)를 그대로
/// 프록시한다. rate limit 은 claude CLI 가 안 내보내지만 `/api/oauth/usage` 가 직접 준다
/// (사용자: ba모드 사용량 패널). 토큰 만료/실패는 그 상태를 ok:false 로 전달.
///
/// **프로세스 전역 TTL 캐시(사용자: "사용량 또 안 뜸")**: oauth/usage 는 레이트리밋이
/// 빡빡해, 터미널 폴러(60초)+웹뷰 TitleBar+여러 pane 이 매 요청 upstream 을 치면 금방
/// 429 로 막힌다. 성공 응답을 캐시해 60초 이내 재요청은 upstream 없이 캐시로 답하고
/// (호출을 60초당 1회로 수렴), upstream 실패(429 등) 시엔 마지막 성공값을 stale 로 돌려
/// pill 이 안 꺼지게 한다. 5시간 창 값이라 수십 초~수 분 stale 은 무해.
/// `GET /claude-identity?dir=<계정 저장소 경로>` — **그 슬롯의 토큰으로** 진짜 신원을
/// 물어본다. `dir` 없음/빈 값 = 기본 로그인.
///
/// 왜 이게 필요한가: `claude auth status` 의 `email`·`orgId`·`orgName` 은 슬롯별
/// 저장소가 아니라 **공유 캐시 `~/.claude.json`** 에서 온다(실측: 공유 캐시를 치우면
/// `loggedIn: true` 인데 email 이 `null`). 그래서 어느 슬롯에 로그인하든 모든 슬롯의
/// 표시 이메일이 방금 로그인한 계정으로 바뀌었다 — 사용자: "계정추가하면 1도 그거로
/// 바뀌어". 저장소는 실제로 갈려 있었고 표시만 거짓말을 하고 있었다.
///
/// 토큰은 이 프로세스 안에서 키체인에서 읽어 헤더로만 나간다 — argv 에 안 실린다
/// (URL 로 오는 건 경로뿐, 비밀이 아니다).
///
/// 슬롯별 TTL 캐시: 설정 화면이 프레임마다 probe 를 부르는 자리라 캐시 없이는
/// upstream 을 두들겨 429 를 부른다. 신원은 거의 안 바뀌므로 5분이면 넉넉하다.
/// 이 금고 dir 이 **활성 계정**의 것인가 — 작업대 지문(workbench-stamp.json)이
/// 정본이다. 활성 계정의 refresh token 사슬은 작업대와 공유(1회용)라, 금고 쪽에서
/// 소비하면 도는 pane 전체가 다음 refresh 에 로그아웃된다(2026-08-18 22:04 실측 —
/// 재시작하자마자 전 pane 이 /login 을 요구했다).
/// 작업대가 429 를 맞았을 때 **대신 물어볼 같은 계정의 금고** 경로.
///
/// 작업대 토큰은 도는 pane 의 claude 들이 다 함께 쓴다 — 각자 한도를 조회하니
/// 호출이 몰려 429 를 맞기 쉽고, 한 번 막히면 화면이 통째로 빈칸이 된다(2026-08-24
/// 실측: 활성 슬롯이 6일째 429). 금고는 **같은 계정의 다른 토큰**이라 한도가 따로
/// 돌고, 돌아오는 숫자는 어차피 같은 계정 것이다.
///
/// ⚠️ **이 폴백은 refresh 를 못 탄다.** `refresh_claude_token` 이 활성 금고를 맨
/// 앞에서 거부하므로(`is_active_vault_dir`) 여기서 나온 경로는 읽기 전용이다. 그게
/// 중요한 이유: 활성 금고를 회전시키면 1회용 refresh token 이 소비돼 **작업대의
/// 사슬이 죽고**, 재시작 때 전 pane 이 로그아웃된다(2026-08-19 실사고). 읽기만
/// 하는 한 그 경로는 열리지 않는다.
fn active_vault_dir() -> Option<String> {
    let home = kasa_socket::home_dir()?;
    let root = home.join(".config/kasaterm/claude-accounts");
    let raw = std::fs::read_to_string(root.join("_active/workbench-stamp.json")).ok()?;
    active_vault_in(&root, &raw)
}

/// 위의 순수부 — 루트와 지문 본문만 받는다. HOME 을 흔들지 않고 검증되어야 하는
/// 이유는 이 함수가 **refresh 금지 규약과 맞물려** 있어서다: 여기서 나온 경로는
/// `is_active_vault_dir` 이 반드시 활성 금고로 알아봐야 하고, 그래야
/// `refresh_claude_token` 이 그 경로를 거부한다. 둘이 어긋나면 조회 폴백이
/// 회전 경로로 새고, 그게 전 세션 로그아웃으로 이어진다.
fn active_vault_in(root: &std::path::Path, stamp_json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(stamp_json).ok()?;
    let acct = v.get("account")?.as_str()?;
    if acct.is_empty() {
        return None;
    }
    Some(root.join(acct).to_string_lossy().into_owned())
}

fn managed_vault_refresh_forbidden(dir: &str) -> bool {
    let path = std::path::Path::new(dir);
    let Some(parent) = path.parent() else { return false };
    let stamp = std::fs::read_to_string(parent.join("_active/workbench-stamp.json")).ok();
    managed_vault_refresh_forbidden_in(path, stamp.as_deref())
}

fn managed_vault_refresh_forbidden_in(path: &std::path::Path, stamp: Option<&str>) -> bool {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else { return false };
    let managed = parent.file_name().is_some_and(|name| name == "claude-accounts") || stamp.is_some();
    if !managed {
        return false;
    }
    // Without ownership evidence any managed vault may share the running workbench's one-use token.
    let owner = stamp.and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .and_then(|v| v.get("account").and_then(|id| id.as_str()).map(str::to_owned));
    match owner.filter(|id| !id.is_empty()) {
        Some(owner) => name == owner.as_str(),
        None => true,
    }
}

async fn claude_identity_handler(
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};
    static CACHE: OnceLock<Mutex<HashMap<String, (Instant, serde_json::Value)>>> = OnceLock::new();
    const TTL: Duration = Duration::from_secs(300);
    let cache = CACHE.get_or_init(Default::default);
    let cors = [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")];

    let dir = params.get("dir").cloned().unwrap_or_default();
    if let Ok(m) = cache.lock() {
        if let Some((at, v)) = m.get(&dir) {
            if at.elapsed() < TTL {
                return (cors, Json(v.clone()));
            }
        }
    }
    // 사용량과 같은 이유로 여기서도 먼저 되살린다 — 신원을 못 읽으면 화면은 라벨만
    // 남고, 그 라벨이 낡았을 때(재로그인으로 슬롯이 겹쳤을 때) 알아챌 길이 사라진다.
    let token = match refresh_claude_token(dir.as_str()).await {
        Some(t) => Some(t),
        None => read_claude_token_from(Some(dir.as_str())),
    };
    // 토큰 문자열이 남아 있어도 갱신이 거부된 채 끝난 슬롯은 로그아웃이다. 그걸
    // 「프로필 일시 실패」로 답하면 화면이 표에 남은 옛 신원으로 「로그인됨」을 그린다.
    let logged_out = claude_slot_login(Some(dir.as_str())) == SlotLogin::LoggedOut;
    let Some(token) = token.filter(|_| !logged_out) else {
        return (cors, Json(serde_json::json!({ "ok": false, "error": "no token" })));
    };
    let resp = reqwest::Client::new()
        .get("https://api.anthropic.com/api/oauth/profile")
        .header("authorization", format!("Bearer {token}"))
        .header("anthropic-beta", "oauth-2025-04-20")
        .send()
        .await;
    let body = match resp {
        Ok(r) if r.status().is_success() => r
            .text()
            .await
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
        _ => None,
    };
    let Some(body) = body else {
        // 여기서 claude 를 그 슬롯으로 띄워 갱신을 시키던 길은 걷었다. 갱신은 위의
        // `refresh_claude_token` 한 곳만 한다 — 갱신하는 쪽이 둘이면 1회용 갱신 토큰을
        // 서로 먼저 쓰려다 슬롯을 죽이고, 진 claude 는 그 자리에 빈 토큰을 써 둔다
        // (2026-10-05 맥북 acct-1·4·8 이 앱 재시작 직후 그 꼴이 됐다).
        // 실패는 캐시하지 않는다 — 네트워크가 돌아오면 바로 진짜 값을 보여야 한다.
        return (cors, Json(serde_json::json!({ "ok": false, "error": "profile api unavailable" })));
    };
    // 응답 어디에 이메일이 들리는지는 버전에 따라 갈리므로 후보를 순서대로 훑는다.
    let pick = |paths: &[&str]| -> Option<String> {
        paths
            .iter()
            .filter_map(|p| body.pointer(p).and_then(|v| v.as_str()))
            .find(|s| !s.is_empty())
            .map(str::to_string)
    };
    let email = pick(&["/account/email_address", "/account/email", "/email_address", "/email"]);
    let org = pick(&["/organization/name", "/account/organization_name", "/organization_name"]);
    // `~/.claude.json` 의 oauthAccount 와 같은 모양(camelCase)으로도 내보낸다 —
    // 계정 전환이 저장소와 함께 이 캐시를 갈아 끼우는 데 쓴다. /status 의
    // Email/Organization 은 토큰이 아니라 이 캐시를 보여주므로(2026-08-16 실측:
    // 파일만 바꿔도 도는 pane 의 /status 가 즉시 따라왔다), 캐시를 안 바꾸면
    // 과금은 새 계정인데 /status 는 옛말을 한다. organizationRole/workspaceRole
    // 은 프로필 응답에 없어 못 채운다 — 표시용 캐시라 비어도 동작엔 지장 없다.
    let account = (email.is_some()).then(|| {
        let g = |p: &str| body.pointer(p).cloned().unwrap_or(serde_json::Value::Null);
        serde_json::json!({
            "accountUuid": g("/account/uuid"),
            "emailAddress": email.clone(),
            "displayName": g("/account/display_name"),
            "accountCreatedAt": g("/account/created_at"),
            "organizationUuid": g("/organization/uuid"),
            "organizationName": org.clone(),
            "organizationType": g("/organization/organization_type"),
            "billingType": g("/organization/billing_type"),
            "organizationRateLimitTier": g("/organization/rate_limit_tier"),
            "seatTier": g("/organization/seat_tier"),
            "hasExtraUsageEnabled": g("/organization/has_extra_usage_enabled"),
            "subscriptionCreatedAt": g("/organization/subscription_created_at"),
        })
    });
    let out = serde_json::json!({ "ok": email.is_some(), "email": email, "org": org, "account": account });
    if out["ok"] == serde_json::Value::Bool(true) {
        if let Ok(mut m) = cache.lock() {
            m.insert(dir, (Instant::now(), out.clone()));
        }
    }
    (cors, Json(out))
}

/// 사용량 조회에 claude CLI 와 같은 User-Agent 를 단다. 서버가 이 값으로 「Claude Code
/// 에서 온 요청」인지 가르고, 아니면 초기화권(`cedar_ember`)을 `ineligible_reason:
/// "surface"` 로 비워 보낸다(2026-09-23 실측: 같은 토큰이 UA 하나로 0장↔1장).
/// 지어낸 값(`claude-cli/0.0.0 (external, kasaterm)`)도 같은 이유로 거절됐다 — 설치된
/// 판 번호를 그대로 쓴다.
fn claude_cli_user_agent() -> &'static str {
    static UA: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    UA.get_or_init(|| {
        let bin = claude_bin();
        let semver = |s: &str| {
            let parts: Vec<&str> = s.split('.').collect();
            parts.len() == 3 && parts.iter().all(|p| p.parse::<u32>().is_ok())
        };
        // 네이티브 설치는 `versions/<판>` 으로 가는 링크라 프로세스를 안 띄우고 읽힌다.
        let version = std::fs::canonicalize(&bin)
            .ok()
            .and_then(|p| p.file_name()?.to_str().map(str::to_string))
            .filter(|n| semver(n))
            .or_else(|| {
                let out = crate::no_window_command(bin.to_string_lossy().as_ref())
                    .arg("--version")
                    .output()
                    .ok()?;
                let text = String::from_utf8(out.stdout).ok()?;
                text.split_whitespace().next().filter(|v| semver(v)).map(str::to_string)
            })
            .unwrap_or_default();
        format!("claude-cli/{version} (external, cli)")
    })
}

async fn claude_usage_handler(
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};
    // 계정 저장소별 (freshness, usage). freshness=Some(at) 면 그 시점 성공값, None 이면
    // 디스크에서 로드한 재시작 이전 값(항상 만료 취급 → upstream 재시도, 실패 시 stale).
    //
    // **캐시를 계정별로 가르는 이유**(사용자 2026-08-05: "누를때마다 바뀐다는 표시가
    // 없고 사용량도 제대로 표기안돼"): 전에는 프로세스 전역 한 벌이라, 계정을 바꿔도
    // 60초 동안은 **떠나온 계정의 숫자**가 그대로 나왔고 upstream 이 막히면 stale
    // 폴백이 그 값을 무한히 이어 줬다. 계정별로 가르면 전환 직후는 캐시 미스라 그
    // 자리에서 새 계정을 조회한다. 실측 당시 세 슬롯의 weekly_all 이 95/25/? 로
    // 제각각인데 화면엔 하나의 숫자만 떴다.
    #[allow(clippy::type_complexity)]
    static CACHE: OnceLock<Mutex<HashMap<String, (Option<Instant>, serde_json::Value)>>> =
        OnceLock::new();
    const TTL: Duration = Duration::from_secs(60);
    let cache = CACHE.get_or_init(Default::default);

    // **토큰별** 「이 시각까지는 이 토큰으로 치지 마라」. 429 를 맞고도 60초마다 계속
    // 두드리면 한도 창이 두드릴 때마다 갱신돼 **영영 안 풀린다** — 실측으로 활성
    // 슬롯이 6일째 429 였고, 그 사이 화면은 내내 빈칸이었다(사용자 2026-08-24
    // "하나도안돼"). 한 번 막히면 물러나 있어야 창이 닫힌다.
    //
    // ⚠️ 키가 **슬롯이 아니라 토큰**인 것이 중요하다. 슬롯으로 걸면 작업대가 막힌
    // 15분 동안 아래 금고 폴백까지 함께 막혀, 폴백을 넣은 의미가 사라진다(실측:
    // 폴백을 넣고도 화면이 여전히 빈칸이었다). 막힌 건 그 토큰이지 계정이 아니다.
    static BACKOFF: OnceLock<Mutex<HashMap<u64, Instant>>> = OnceLock::new();
    const BACKOFF_FOR: Duration = Duration::from_secs(15 * 60);
    let backoff = BACKOFF.get_or_init(Default::default);

    // 조회 대상 슬롯: `?dir=` 이 있으면 그것, 없으면 활성 계정(kasaterm 이 shim 을
    // 깔 때마다 `KASATERM_CLAUDE_ACCOUNT_DIR` 로 알려 준다). 빈 문자열 = 기본 로그인.
    let dir = params
        .get("dir")
        .cloned()
        .unwrap_or_else(|| std::env::var("KASATERM_CLAUDE_ACCOUNT_DIR").unwrap_or_default());
    // The shared workbench keeps the same empty runtime directory across
    // account switches. Cache by its stamped account, never by that directory.
    let cache_slot = usage_cache_slot(&dir, active_vault_dir().as_deref());

    let cors = [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")];
    // `account_dir` 을 함께 돌려준다 — 어느 계정의 숫자인지 소비자가 알 수 있어야
    // 전환 직후 옛 값을 새 계정 것으로 오인하지 않는다.
    let ok = |v: &serde_json::Value, stale: bool| {
        serde_json::json!({ "ok": true, "usage": v, "stale": stale, "account_dir": dir })
    };

    // `?fresh=1` 이면 신선 캐시도 건너뛴다 — 계정 목록을 **펼쳐 놓고 보는 동안**
    // 쓰는 문이다(2026-08-27 지시 「누르면 펼쳐지잖아 거기 업데이트 되게하라니까」).
    // 60초 TTL 은 닫혀 있을 때는 맞다: 그때 이 값을 읽는 것은 상태줄 한 줄뿐이라
    // 1분 낡아도 판단이 안 갈린다. 하지만 목록을 열어 둔 사람은 **지금 어디로
    // 옮길지**를 고르는 중이고, 그 화면에서 숫자가 1분 내리 굳어 있으면 갱신이
    // 죽은 것으로 읽힌다.
    //
    // 백오프(429)는 **우회하지 않는다** — 그건 upstream 이 그만 두드리라고 한
    // 것이고, 화면이 열려 있다는 사정과 무관하다. 아래 3) 이 그대로 처리한다.
    let fresh = params.get("fresh").is_some_and(|v| v == "1" || v == "true");
    // 1) 신선한 캐시(60초 이내 성공)면 upstream 없이 그대로.
    if let Ok(g) = cache.lock() {
        if let Some((Some(at), v)) = g.get(&cache_slot) {
            if !fresh && at.elapsed() < TTL {
                return (cors, Json(ok(v, false)));
            }
        }
    }

    // 2) 신선 캐시가 없을 때만 upstream 시도. 첫 조회면 디스크 스냅샷을 먼저 실어
    //    둔다 — 재시작 직후 upstream 이 429 면 3) 이 그걸 stale 로 돌려줘 pill 이
    //    빈칸으로 떨어지지 않는다(사용자: "사용량 또 안 뜸").
    {
        let mut seed = None;
        if let Ok(g) = cache.lock() {
            if !g.contains_key(&cache_slot) {
                seed = load_usage_disk(&cache_slot);
            }
        }
        if let Some(v) = seed {
            if let Ok(mut g) = cache.lock() {
                g.entry(cache_slot.clone()).or_insert((None, v));
            }
        }
    }
    // 만료됐으면 먼저 되살린다. 안 그러면 안 쓰는 슬롯은 영영 401 이고, 화면엔
    // 「모름(—)」만 남아 정작 옮길 곳을 고를 때 아무 도움이 안 된다.
    let token = match refresh_claude_token(dir.as_str()).await {
        Some(t) => Some(t),
        None => read_claude_token_from(Some(dir.as_str())),
    };
    // 왜 못 읽었는지를 남긴다. 전에는 어떤 실패든 「rate-limited」 한 문구라, 정작
    // 로그아웃된 슬롯도 「한도 초과」로 보여 사용자가 기다리면 될 줄 알았다.
    let mut why = if token.is_some() { "usage_unavailable" } else { "logged_out" };
    // 토큰 후보. 작업대(빈 dir)가 429 면 같은 계정의 금고로 한 번 더 묻는다 —
    // 다른 토큰이라 한도가 따로 돌고, 숫자는 어차피 같은 계정 것이다.
    let mut tokens: Vec<String> = token.into_iter().collect();
    if dir.is_empty() {
        if let Some(vault) = active_vault_dir().and_then(|d| read_claude_token_from(Some(&d))) {
            if !tokens.contains(&vault) {
                tokens.push(vault);
            }
        }
    }
    let mut fresh: Option<serde_json::Value> = None;
    for token in &tokens {
        // 막힌 동안은 이 토큰으로 아예 안 친다 — 두드림 자체가 한도 창을 되살린다.
        // 다음 후보는 다른 토큰이라 그대로 시도한다.
        let key = token_key(token);
        let blocked = backoff
            .lock()
            .ok()
            .and_then(|g| g.get(&key).copied())
            .is_some_and(|until| Instant::now() < until);
        if blocked {
            why = "rate_limited";
            continue;
        }
        // `cedar_ember=1` 이 한도 초기화권 잔량을 함께 싣는다(limit_reset.rs 가 읽는다).
        let resp = reqwest::Client::new()
            .get("https://api.anthropic.com/api/oauth/usage?cedar_ember=1")
            .header("authorization", format!("Bearer {token}"))
            .header("anthropic-beta", "oauth-2025-04-20")
            .header("user-agent", claude_cli_user_agent())
            .send()
            .await;
        match resp {
            Ok(r) if r.status().is_success() => {
                if let Ok(mut g) = backoff.lock() {
                    g.remove(&key);
                }
                fresh = r
                    .text()
                    .await
                    .ok()
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
                if fresh.is_some() {
                    break;
                }
            }
            Ok(r) => {
                let code = r.status().as_u16();
                why = match code {
                    429 => "rate_limited",
                    401 | 403 => "token_rejected",
                    _ => "usage_unavailable",
                };
                if code == 429 {
                    if let Ok(mut g) = backoff.lock() {
                        g.insert(key, Instant::now() + BACKOFF_FOR);
                    }
                }
                // 이 슬롯의 접근 토큰이 죽었다 — 기기엔 갱신 토큰이 없어(관문이 쥔다) claude 가 스스로 못 살린다.
                // 5분 주기를 기다리면 그동안 칸마다 「OAuth token revoked」로 멈춘다(2026-10-09 실측).
                if matches!(code, 401 | 403) {
                    crate::agent_chains::poke();
                }
                // 상태만 남긴다(토큰은 절대). 조용한 실패는 현장에서 못 가른다.
                eprintln!("[claude-usage] upstream {code} — slot={}", slot_label(&dir));
            }
            Err(e) => {
                why = "network";
                eprintln!("[claude-usage] 요청 실패: {e}");
            }
        }
    }
    // Do not publish or persist an answer fetched across an account switch.
    if usage_cache_slot(&dir, active_vault_dir().as_deref()) != cache_slot {
        return (cors, Json(serde_json::json!({
            "ok": false, "reason": "account_changed", "account_dir": dir,
        })));
    }
    if let Some(v) = fresh {
        if let Ok(mut g) = cache.lock() {
            g.insert(cache_slot.clone(), (Some(Instant::now()), v.clone()));
        }
        save_usage_disk(&cache_slot, &v);
        return (cors, Json(ok(&v, false)));
    }

    // 3) upstream 실패 — 만료됐어도 이 슬롯의 마지막 성공값이 있으면 stale 로
    //    폴백(pill 유지). **다른 슬롯 값으로는 절대 폴백하지 않는다** — 그게 전에
    //    한 계정의 숫자를 세 계정에 전부 붙여 보이던 경로다.
    if let Ok(g) = cache.lock() {
        if let Some((_, v)) = g.get(&cache_slot) {
            return (cors, Json(ok(v, true)));
        }
    }
    // 갱신이 죽은 슬롯은 429 를 맞았더라도 **로그인 문제**다 — 기다려서 안 풀린다.
    if dead_refresh().lock().is_ok_and(|g| g.contains(&dir)) {
        why = "token_rejected";
    }
    let msg = match why {
        "rate_limited" => "한도 조회가 잠시 막혔어요 — 곧 다시 시도해요",
        "token_rejected" => "로그인이 만료됐어요 — 다시 로그인해 주세요",
        "logged_out" => "로그인이 풀렸어요 — 다시 로그인해 주세요",
        "network" => "네트워크가 닿지 않아요",
        _ => "한도를 못 읽었어요",
    };
    (
        cors,
        Json(serde_json::json!({
            "ok": false, "error": msg, "reason": why, "account_dir": dir,
        })),
    )
}

fn usage_cache_slot(dir: &str, active_vault: Option<&str>) -> String {
    if !dir.is_empty() {
        return dir.to_string();
    }
    // Old empty-dir snapshots have no account provenance and may belong to a
    // previously selected slot. Even the default login gets a new explicit key.
    active_vault
        .filter(|slot| !slot.is_empty())
        .unwrap_or("@default-login")
        .to_string()
}

/// 갱신이 **400/401 로 거부된** 슬롯. 그건 refresh token 이 죽었다는 뜻이라
/// 기다려서 풀리지 않는다 — 다시 로그인해야만 산다.
///
/// 이걸 따로 기억하는 이유: 그 뒤 usage 호출이 429 를 맞으면 표시가 「한도 조회가
/// 막혔어요」가 되어 **기다리면 될 것처럼 보인다**(2026-08-25 실측: 네이버 슬롯이
/// 정확히 그 모양이었다 — 갱신 400 인데 화면은 한도 초과라고 말했다). 죽은 로그인이
/// 429 뒤에 숨지 않게, 이쪽을 먼저 말한다.
fn dead_refresh() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static DEAD: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    DEAD.get_or_init(Default::default)
}

/// 백오프 맵의 키. 토큰 문자열을 그대로 키로 두면 값이 맵에 오래 남으므로 지문만
/// 쓴다 — 같은 토큰인지만 알면 되고, 되돌릴 필요가 없다.
fn token_key(token: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    token.hash(&mut h);
    h.finish()
}

/// 로그에 쓸 슬롯 이름. 경로 전체는 홈 디렉터리가 통째로 찍혀 길기만 하다.
fn slot_label(dir: &str) -> &str {
    if dir.is_empty() {
        return "(작업대)";
    }
    dir.rsplit('/').next().unwrap_or(dir)
}

/// `~/.config/kasaterm/usage-cache.json` — 계정 저장소별 마지막 성공 스냅샷 한 파일.
/// 슬롯 경로를 키로 쓰므로 계정을 늘려도 파일이 안 늘고, 계정을 지워도 남은 항목이
/// 다른 계정 숫자로 새지 않는다.
fn usage_cache_path() -> Option<std::path::PathBuf> {
    let home = kasa_socket::home_dir()?;
    Some(home.join(".config/kasaterm/usage-cache.json"))
}

/// 스냅샷에서 `dir` 슬롯의 usage 본문을 꺼낸다 — **하루 이내** 기록만.
///
/// 처음엔 6시간이었다(5시간 창이 만료되면 폐기). 그런데 upstream 이 오래 막히면
/// 그 규칙이 화면을 통째로 비운다 — 낡은 「~71% 씀」이 **아무것도 없는 것보다**
/// 훨씬 낫다(2026-08-24: 활성 슬롯이 6일째 429 라 게이지가 내내 빈칸이었고, 그게
/// 「기능이 하나도 안 된다」로 읽혔다). 호출부가 `stale` 을 함께 받아 `~` 를 붙이니
/// 사용자도 옛 값인 줄 안다.
///
/// 파일 IO 를 밖에 두는 이유는 이 판정이 **한 계정의 숫자를 다른 계정에 붙이지
/// 않는가**를 결정하는 자리라, HOME 을 흔들지 않고 검증돼야 해서다.
fn usage_from_snapshot(json: &str, dir: &str, now: u64) -> Option<serde_json::Value> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let fresh = |e: &serde_json::Value| -> Option<serde_json::Value> {
        let ts = e.get("ts")?.as_u64()?;
        (now.saturating_sub(ts) <= 24 * 3600).then(|| e.get("usage").cloned())?
    };
    // 새 형식: { slots: { "<dir>": {ts, usage} } }. 옛 형식({ts, usage})은 어느 계정
    // 것인지 기록이 없으므로 **기본 슬롯(빈 dir)일 때만** 받아들인다 — 그러지 않으면
    // 업그레이드 직후 한 번, 옛 계정 숫자가 새 계정 자리에 그대로 앉는다.
    if let Some(slot) = v.pointer("/slots").and_then(|s| s.get(dir)) {
        return fresh(slot);
    }
    if dir.is_empty() && v.get("ts").is_some() {
        return fresh(&v);
    }
    None
}

/// 디스크 캐시 로드 — `dir` 슬롯 항목만 본다. 프로세스 전역 한 벌이던 옛 구조는
/// 재시작 직후 활성 계정에 **떠나온 계정의** 스냅샷을 붙여 줬다.
fn load_usage_disk(dir: &str) -> Option<serde_json::Value> {
    let s = std::fs::read_to_string(usage_cache_path()?).ok()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    usage_from_snapshot(&s, dir, now)
}

/// 기존 스냅샷 문서에 `dir` 슬롯을 갱신해 되쓸 문서를 만든다. 다른 슬롯 항목은
/// 그대로 살려 둔다 — 계정을 옮겨 다녀도 각자의 마지막 값이 남는다.
fn merge_usage_snapshot(
    existing: Option<&str>,
    dir: &str,
    usage: &serde_json::Value,
    now: u64,
) -> String {
    let mut slots = existing
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .and_then(|v| v.get("slots").cloned())
        .and_then(|s| s.as_object().cloned())
        .unwrap_or_default();
    slots.insert(dir.to_string(), serde_json::json!({ "ts": now, "usage": usage }));
    serde_json::json!({ "slots": slots }).to_string()
}

/// 성공 usage 본문을 ts 와 함께 디스크에 저장(재시작 폴백 소스).
fn save_usage_disk(dir: &str, usage: &serde_json::Value) {
    let Some(p) = usage_cache_path() else { return };
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let existing = std::fs::read_to_string(&p).ok();
    let _ = std::fs::write(p, merge_usage_snapshot(existing.as_deref(), dir, usage, now));
}

/// 이 모듈 창구의 라우트. `router` 가 한 표로 합친 뒤 공통 레이어(Origin·토큰 가드)를 두른다.
pub(super) fn routes() -> axum::Router {
    axum::Router::new()
        .route("/claude-usage", get(claude_usage_handler))
        .route("/claude-identity", get(claude_identity_handler))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 계정 저장소별로 스냅샷이 갈리는지 — 한 계정의 숫자가 다른 계정 자리에 앉으면
    /// 한도 분산 기능에서 한도 표시가 거짓말을 한다(사용자 2026-08-05: 세 계정의
    /// weekly_all 이 95/25/? 인데 화면엔 하나의 숫자만 떴다).
    #[test]
    fn usage_snapshot_is_per_account_slot() {
        let now = 1_785_000_000u64;
        let a = serde_json::json!({ "limits": [{ "group": "weekly", "percent": 95 }] });
        let b = serde_json::json!({ "limits": [{ "group": "weekly", "percent": 25 }] });
        let doc = merge_usage_snapshot(None, "", &a, now);
        let doc = merge_usage_snapshot(Some(&doc), "/slots/acct-1", &b, now);
        // 각 슬롯이 자기 값을 돌려주고, 서로 섞이지 않는다.
        assert_eq!(usage_from_snapshot(&doc, "", now), Some(a));
        assert_eq!(usage_from_snapshot(&doc, "/slots/acct-1", now), Some(b));
        // 기록이 없는 슬롯은 **다른 슬롯 값으로 폴백하지 않는다** — 빈 값이 틀린 값보다 낫다.
        assert_eq!(usage_from_snapshot(&doc, "/slots/acct-2", now), None);
    }

    #[test]
    fn shared_workbench_usage_follows_account_identity_and_rejects_unowned_history() {
        let now = 1_785_000_000u64;
        let account_a = usage_cache_slot("", Some("/slots/acct-a"));
        let account_b = usage_cache_slot("", Some("/slots/acct-b"));
        let default = usage_cache_slot("", None);
        let old = serde_json::json!({"limits":[{"percent":3}]});
        let a = serde_json::json!({"limits":[{"percent":52}]});
        let b = serde_json::json!({"limits":[{"percent":71}]});
        let doc = merge_usage_snapshot(None, "", &old, now);
        assert_eq!(usage_from_snapshot(&doc, &account_a, now), None);
        assert_eq!(usage_from_snapshot(&doc, &default, now), None);
        let doc = merge_usage_snapshot(Some(&doc), &account_a, &a, now);
        assert_eq!(usage_from_snapshot(&doc, &account_b, now), None);
        let doc = merge_usage_snapshot(Some(&doc), &account_b, &b, now);
        assert_eq!(usage_from_snapshot(&doc, &account_a, now), Some(a));
        assert_eq!(usage_from_snapshot(&doc, &account_b, now), Some(b));
        assert_eq!(
            usage_cache_slot("/slots/acct-a", Some("/slots/acct-b")),
            account_a
        );
        assert_eq!(usage_cache_slot("", Some("")), default);
    }

    /// 낡은 값이라도 하루까지는 살린다 — upstream 이 오래 막혔을 때 빈칸보다
    /// 「~71% 씀」이 낫다. 다만 무한정은 아니다: 며칠 전 숫자를 지금 것처럼
    /// 그리면 옮길 곳을 고르는 판단이 통째로 틀어진다.
    /// 조회 폴백이 만드는 경로는 **반드시** 활성 금고로 인식돼야 한다 — 그래야
    /// `refresh_claude_token` 이 첫 줄에서 거부하고, 폴백이 회전 경로로 새지 않는다.
    /// 이 대칭이 깨지면 활성 금고의 1회용 refresh token 이 소비돼 작업대 사슬이
    /// 죽고, 재시작 때 전 pane 이 로그아웃된다(2026-08-19 실사고).
    #[test]
    fn the_usage_fallback_path_is_always_refresh_forbidden() {
        let tmp = std::env::temp_dir().join(format!("kasa-vault-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("_active")).unwrap();
        let stamp = r#"{"account":"acct-5","digest":"deadbeef"}"#;
        std::fs::write(tmp.join("_active/workbench-stamp.json"), stamp).unwrap();

        let vault = active_vault_in(&tmp, stamp).expect("지문이 계정을 말하면 경로가 나온다");
        assert!(vault.ends_with("acct-5"));
        assert!(
            managed_vault_refresh_forbidden(&vault),
            "폴백 경로가 활성 금고로 안 보이면 refresh 거부를 통과해 버린다"
        );

        // 다른 슬롯은 활성이 아니다 — 그쪽은 회전해도 작업대와 무관하다.
        let other = tmp.join("acct-1").to_string_lossy().into_owned();
        assert!(!managed_vault_refresh_forbidden(&other));

        // 지문에 계정이 없으면 폴백 자체가 없다(빈 경로를 만들어 기본 슬롯을
        // 두 번 치는 일이 없어야 한다).
        assert_eq!(active_vault_in(&tmp, r#"{"account":""}"#), None);
        assert_eq!(active_vault_in(&tmp, "{}"), None);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn unidentified_managed_workbench_forbids_vault_refresh() {
        let vault = std::path::Path::new("/test/claude-accounts/acct-1");
        for stamp in [None, Some("{}"), Some(r#"{"account":""}"#), Some("invalid")] {
            assert!(managed_vault_refresh_forbidden_in(vault, stamp));
        }
        assert!(managed_vault_refresh_forbidden_in(vault, Some(r#"{"account":"acct-1"}"#)));
        assert!(!managed_vault_refresh_forbidden_in(vault, Some(r#"{"account":"acct-2"}"#)));
        assert!(!managed_vault_refresh_forbidden_in(std::path::Path::new(""), None));
        assert!(!managed_vault_refresh_forbidden_in(std::path::Path::new("/test/standalone"), None));
    }

    #[test]
    fn usage_snapshot_survives_a_day_then_expires() {
        let saved_at = 1_785_000_000u64;
        let doc = merge_usage_snapshot(None, "", &serde_json::json!({ "x": 1 }), saved_at);
        assert!(usage_from_snapshot(&doc, "", saved_at + 6 * 3600).is_some(), "6시간은 당연히 유효");
        assert!(usage_from_snapshot(&doc, "", saved_at + 24 * 3600).is_some(), "하루 경계는 유효");
        assert!(usage_from_snapshot(&doc, "", saved_at + 24 * 3600 + 1).is_none(), "그 뒤는 폐기");
    }

    /// 업그레이드 경로 — 옛 형식(`{ts, usage}`)은 어느 계정 것인지 기록이 없다.
    /// 기본 슬롯일 때만 받아들이고, 이름 붙은 슬롯에는 절대 붙이지 않는다.
    #[test]
    fn legacy_flat_snapshot_only_feeds_the_default_slot() {
        let now = 1_785_000_000u64;
        let legacy = serde_json::json!({ "ts": now, "usage": { "limits": [] } }).to_string();
        assert!(usage_from_snapshot(&legacy, "", now).is_some());
        assert!(usage_from_snapshot(&legacy, "/slots/acct-1", now).is_none());
    }

    /// Pins the naming to Claude Code's own scheme. Expected values come from
    /// `printf %s <path> | shasum -a 256`, which is what the CLI computes — if
    /// this drifts, the usage pill silently falls back to reading nothing.
    #[test]
    fn keychain_service_matches_claudes_hashing() {
        assert_eq!(
            claude_keychain_service(Some("/tmp/acct/a1")),
            "Claude Code-credentials-63cab202"
        );
        // 미선택·빈 문자열은 둘 다 "접미사 없는 기본 저장소"다. 빈 문자열을 해시하면
        // e3b0c442(빈 입력의 sha256)라는 그럴듯한 이름이 나와 조용히 빗나간다.
        assert_eq!(claude_keychain_service(None), "Claude Code-credentials");
        assert_eq!(claude_keychain_service(Some("")), "Claude Code-credentials");
    }

    /// claude 가 갱신 거부 뒤 남기는 빈 토큰 자리를 토큰으로 읽으면 안 된다.
    #[test]
    fn wiped_slot_has_no_token_and_is_signed_out() {
        let d = temp_dir("acct-wiped");
        std::fs::write(
            d.join(".credentials.json"),
            r#"{"claudeAiOauth":{"accessToken":"","refreshToken":"","expiresAt":0,"subscriptionType":"max"}}"#,
        )
        .unwrap();
        assert_eq!(read_claude_token_from(Some(d.to_str().unwrap())), None);
        let doc = serde_json::from_str(&std::fs::read_to_string(d.join(".credentials.json")).unwrap()).unwrap();
        assert!(!oauth_usable(&doc, 1, false));
    }

    #[test]
    fn slot_login_follows_refresh_token_and_server_rejection() {
        let doc = |access: &str, refresh: &str, exp: u64| {
            serde_json::json!({"claudeAiOauth": {"accessToken": access, "refreshToken": refresh, "expiresAt": exp}})
        };
        assert!(oauth_usable(&doc("a", "r", 10), 100, false), "만료돼도 갱신 토큰이 있으면 로그인");
        assert!(!oauth_usable(&doc("a", "r", 10), 100, true), "갱신 토큰이 거부됐고 access 도 끝났으면 로그아웃");
        assert!(oauth_usable(&doc("a", "r", 1000), 100, true), "거부됐어도 access 가 살아 있는 동안은 로그인");
        assert!(oauth_usable(&doc("a", "", 1000), 100, false));
        assert!(!oauth_usable(&doc("a", "", 10), 100, false));
    }

    #[test]
    fn one_refresh_turn_per_slot() {
        let a = refresh_gate("/x/acct-1");
        assert!(std::sync::Arc::ptr_eq(&a, &refresh_gate("/x/acct-1")), "같은 슬롯은 같은 줄에 선다");
        assert!(!std::sync::Arc::ptr_eq(&a, &refresh_gate("/x/acct-2")), "다른 슬롯은 서로 안 기다린다");
    }

    /// 계정을 고르면 그 저장소에서 토큰을 읽어야 한다 — 안 그러면 pill 이 계정을
    /// 바꿔도 기본 계정 한도를 계속 보여준다.
    #[test]
    fn token_comes_from_the_selected_account_store() {
        let d = temp_dir("acct-token");
        std::fs::write(
            d.join(".credentials.json"),
            r#"{"claudeAiOauth":{"accessToken":"tok-from-account"}}"#,
        )
        .unwrap();
        assert_eq!(
            read_claude_token_from(Some(d.to_str().unwrap())),
            Some("tok-from-account".to_string())
        );
    }
}
