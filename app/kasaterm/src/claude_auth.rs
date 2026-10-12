//! 재시작 없는 계정 전환 — claude 가 **매 요청 직전 다시 읽는** 자격증명 저장소를
//! 밖에서 갈아 끼운다.
//!
//! ## 왜 이게 되나 (claude 2.1.233 실물 확인, 2026-08-15)
//!
//! 계정은 프로세스 env 라 도는 claude 의 계정을 밖에서 못 바꾼다 — 그래서 전에는
//! pane 을 재시작했다. 그런데 claude 는 자격증명을 **부팅 때 한 번 읽고 마는 것이
//! 아니다**: 요청 프리플라이트(`refreshOAuthTokenIfNeeded`)와 401 복구에서 캐시를
//! 비우고 저장소를 다시 읽고, 저장소에 자기 것과 다른 access token 이 있으면 네트워크
//! 갱신도 없이 **그것을 그대로 채택**한다. 그러니 그 자리의 내용물을 바꿔 두면 도는
//! 세션이 다음 메시지부터 새 계정으로 말한다. 지연 상한은 keychain 캐시 30초.
//!
//! ## 자리 이름 규칙 (claude 의 `WZ()` 재현)
//!
//! macOS keychain 서비스명 = `Claude Code-credentials` + 접미. `CLAUDE_SECURESTORAGE_
//! CONFIG_DIR` 이 비어 있지 않으면 접미 `-<sha256(경로 NFC).hex[..8]>` 가 **강제로**
//! 붙는다. 실측으로 확인: `~/.config/kasaterm/claude-accounts/acct-1` → `77d5ac7d`,
//! `acct-2` → `2d12f0b3` … 다섯 슬롯이 사용자 keychain 의 항목명과 정확히 일치했다.
//!
//! ## 구조 — 금고와 작업대를 가른다 (Orca 와 같은 규칙)
//!
//! - **금고**(vault): 계정마다 하나. `claude-accounts/acct-N` 슬롯. 정본이다.
//! - **작업대**: claude 의 **기본 자리**(env 없이 뜰 때 쓰는 접미 없는 keychain
//!   항목 / `~/.claude/.credentials.json`). pane 의 claude 는 env 없이 떠서 전부
//!   여기를 보고 돈다. 전환 = 금고에서 기본 자리로 옮겨 담기. Orca 가 정확히
//!   이렇게 한다(선택한 계정을 공유 기본 위치로 실어 나른다).
//!
//! **작업대가 왜 기본 자리여야 하나 (2026-08-16 실측).** 처음엔 전용 슬롯
//! `_active` 를 만들었는데, 그 keychain 항목은 우리가 `security` CLI 로 만든
//! 것이라 파티션이 `apple-tool:` 뿐이다. claude 는 Security **프레임워크**
//! (`SecItemCopyMatching`, 바이너리에서 확인)로 여는데, CLI 가 만든 항목을
//! 프레임워크로 열면 macOS 가 암호 창을 띄운다 — 앱을 켤 때마다 사용자가 암호를
//! 쳐야 했다. `-A`(모든 앱 허용) ACL 로도 못 재운다(프레임워크 읽기로 실측 —
//! 파티션 검사가 ACL 보다 먼저다). 파티션은 keychain 암호 없이는 CLI 로 못
//! 바꾼다. 유일하게 전 조합이 조용한 자리가 **claude 자신이 로그인 때 만든 기본
//! 항목**이다: claude(프레임워크)는 제 항목이라 조용하고, 우리(CLI)는 apple-tool
//! 파티션이라 조용하다 — 읽기·`-U` 갱신 둘 다 같은 값 재기록으로 실측했다.
//!
//! 기본 자리에 원래 있던 로그인은 처음 한 번 `claude-accounts/_default-backup`
//! 금고로 떠 둔다 — 등록 안 된 계정이었어도 전환 한 번에 유실되지 않는다.
//!
//! ## refresh token 은 1회용이다 — 되받기가 없으면 계정이 죽는다
//!
//! 도는 claude 는 토큰이 만료되면 스스로 갱신하고 **작업대에 새 토큰을 쓴다**. 그때
//! 금고의 것은 이미 쓴 refresh token 이라 죽은 값이 된다. 그 상태로 다음에 금고에서
//! 다시 꺼내 덮으면 그 계정은 로그아웃된다. 그래서 작업대가 우리가 마지막으로 쓴 것과
//! 달라졌으면(=도는 세션이 갱신했으면) **먼저 금고로 되받아** 정본을 갱신한다.
//! Orca 가 같은 이유로 read-back 을 둔다(`readBackRefreshedTokens`).
//!
//! 지문은 값이 아니라 **해시**만 남긴다 — 토큰을 우리 파일에 두 벌로 늘리지 않는다.

use std::path::{Path, PathBuf};
pub(crate) mod phone_credentials;

/// 지문 파일이 사는 폴더 이름. 계정 id 로는 못 쓰는 이름이라(슬롯 id 는 `acct-N`)
/// 실제 계정과 충돌하지 않는다. 자격증명은 여기 살지 않는다 — 작업대는 claude 의
/// 기본 자리다(모듈 머리말).
pub(crate) const ACTIVE_SLOT: &str = "_active";

fn credential_mutation() -> std::sync::MutexGuard<'static, ()> {
    // The usage worker must not read back a half-finished GUI account switch.
    static MUTATION: std::sync::Mutex<()> = std::sync::Mutex::new(());
    MUTATION.lock().unwrap_or_else(|error| error.into_inner())
}

/// 처음 전환하기 전에 기본 자리에 있던 로그인을 떠 두는 금고. 등록 안 된 계정이라도
/// 전환 한 번에 유실되지 않게 — 복구는 이 슬롯을 읽으면 된다.
const DEFAULT_BACKUP_SLOT: &str = "_default-backup";

/// 작업대의 실제 저장소 — claude 가 env 없이 뜰 때 쓰는 기본 자리. macOS 는 접미
/// 없는 keychain 항목(`None`), 그 밖은 `~/.claude/.credentials.json`.
fn workbench_store() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        None
    } else {
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(|h| PathBuf::from(h).join(".claude"))
    }
}

/// keychain 서비스 접두 — claude 의 `WZ()` 가 만드는 이름과 같아야 한다.
const SERVICE_BASE: &str = "Claude Code-credentials";

/// 계정 저장소 경로 → claude 가 그 계정으로 볼 keychain 서비스명.
///
/// `None` = 기본 로그인(env 미설정) → 접미 없는 이름. 그 자리는 사용자가 kasaterm
/// 밖에서 쓰는 로그인이라 **우리가 쓰지 않는다**(읽기만).
pub(crate) fn service_name(dir: Option<&Path>) -> String {
    let Some(dir) = dir.filter(|d| !d.as_os_str().is_empty()) else {
        return SERVICE_BASE.to_string();
    };
    use sha2::{Digest, Sha256};
    use unicode_normalization::UnicodeNormalization;
    // claude 는 경로를 NFC 로 정규화한 뒤 해시한다. 한글이 든 경로에서 갈린다 —
    // 맥 파일시스템이 NFD 로 주기 때문에 이 한 줄이 없으면 이름이 어긋난다.
    let nfc: String = dir.to_string_lossy().nfc().collect();
    let hex = format!("{:x}", Sha256::digest(nfc.as_bytes()));
    format!("{SERVICE_BASE}-{}", &hex[..8])
}

/// 지문이 사는 폴더. `settings.json` 이 있는 폴더 아래라 헤드리스 검증이 스크래치
/// 설정을 가리키면 지문도 스크래치로 따라간다(계정 폴더와 같은 규칙).
pub(crate) fn active_dir() -> Option<PathBuf> {
    crate::socket::claude_account_dir(ACTIVE_SLOT)
}

/// 지문 파일 — 「우리가 작업대에 마지막으로 써 넣은 것」의 해시와 그때의 계정 id.
/// 이 둘이 있어야 「도는 세션이 갱신한 것」과 「우리가 방금 넣은 것」을 가른다.
/// 파일명이 곧 판 구분이다. `_active` 전용 슬롯 시절의 지문(`active-stamp.json`)을
/// 그대로 읽으면, 기본 자리에 아직 옛 로그인이 있는데 지문은 「계정 X 가 작업대에
/// 있다」고 우겨 pane 이 엉뚱한 계정으로 돈다 — 새 이름이라 옛 지문은 자연히
/// 무시되고 첫 전환이 작업대를 새로 채운다.
fn stamp_path_in(active: &Path) -> PathBuf {
    active.join("workbench-stamp.json")
}

/// 자격증명 한 벌의 **만료 시각**(epoch ms). 어느 쪽이 최신인지 가르는 유일한 단서라
/// 값 전체를 비교하지 않고 이 한 숫자만 꺼낸다 — 토큰은 절대 밖으로 안 나간다.
///
/// 두 자리(금고·작업대)가 잠깐 공존하는 구간이 있고, refresh token 은 1회용이라
/// **오래된 쪽으로 덮으면 그 계정이 로그아웃된다.** 그래서 되받기는 언제나 「더 새것이
/// 이긴다」로 판정한다.
/// 그 blob 이 **쓸 만한 로그인**인가. 만료 시각이 없거나 0 이면 로그인 안 된
/// 껍데기다(실측: 금고 하나가 `expiresAt: 0` 으로 저장돼 있었다).
///
/// 판정을 여기 한 곳에 두는 이유: `swap_active` 가 이걸로 `VaultEmpty` 를 정하고,
/// 전환 **전** 미리보기(`vault_ready`)가 같은 답을 내야 「물어본 내용」과 「실제로
/// 벌어지는 일」이 안 갈린다. 두 곳에 같은 식을 쓰면 언젠가 한쪽만 고쳐진다.
fn blob_alive(blob: &[u8]) -> bool {
    expires_at(blob).is_some_and(|e| e > 0)
}

/// 그 계정 금고에 쓸 만한 로그인이 들어 있나 — `swap_active` 가 `VaultEmpty` 로
/// 물러나는 조건과 **같은 판정**이다. 읽기만 하므로 전환 전에 불러도 안전하다.
pub(crate) fn vault_ready(
    account_id: &str,
    vault_dir_of: impl Fn(&str) -> Option<PathBuf>,
) -> bool {
    if account_id.is_empty() {
        return false;
    }
    let vault = vault_dir_of(account_id);
    read_credentials(vault.as_deref()).is_some_and(|b| blob_alive(&b))
}

fn expires_at(blob: &[u8]) -> Option<i64> {
    let v: serde_json::Value = serde_json::from_slice(blob).ok()?;
    v.pointer("/claudeAiOauth/expiresAt")
        .and_then(|x| x.as_i64())
        .or_else(|| v.get("expiresAt").and_then(|x| x.as_i64()))
}

/// `b` 가 `a` 보다 새것인가. 만료 시각을 못 읽으면 **덮지 않는 쪽**으로 판정한다 —
/// 모르는 값으로 로그인을 밀어내는 것보다 그냥 두는 편이 안전하다.
fn is_newer(a: &[u8], b: &[u8]) -> bool {
    match (expires_at(a), expires_at(b)) {
        (Some(x), Some(y)) => y > x,
        _ => false,
    }
}

fn digest_of(blob: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(blob))
}

fn read_stamp_in(active: &Path) -> Option<(String, String)> {
    let raw = std::fs::read_to_string(stamp_path_in(active)).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    Some((
        v.get("account")?.as_str()?.to_string(),
        v.get("digest")?.as_str()?.to_string(),
    ))
}

#[cfg(test)]
fn write_stamp_in(active: &Path, account: &str, digest: &str) {
    write_stamp_with_identity_in(active, account, digest, None);
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct IdentityKey {
    account_uuid: String,
    organization_uuid: String,
}

impl IdentityKey {
    fn from_verified(identity: kasa_mcp::VerifiedClaudeIdentity) -> Self {
        Self { account_uuid: identity.account_uuid, organization_uuid: identity.organization_uuid }
    }
}

fn blob_identity_key(blob: &[u8]) -> Option<IdentityKey> {
    verified_blob_identity(blob).map(IdentityKey::from_verified)
}

fn read_stamp_identity_in(active: &Path) -> Option<IdentityKey> {
    let raw = std::fs::read_to_string(stamp_path_in(active)).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    serde_json::from_value(value.get("identity")?.clone()).ok()
}

fn write_stamp_with_identity_in(active: &Path, account: &str, digest: &str, identity: Option<&IdentityKey>) {
    let p = stamp_path_in(active);
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let v = serde_json::json!({ "account": account, "digest": digest, "identity": identity });
    let _ = std::fs::write(p, v.to_string());
}

/// 자격증명 한 벌을 읽는다. macOS 는 keychain, 그 밖은 `<dir>/.credentials.json`.
/// 값은 호출부 메모리에만 머문다 — 로그에도 화면에도 내보내지 않는다.
pub(crate) fn read_credentials(dir: Option<&Path>) -> Option<Vec<u8>> {
    // ⚠️ 시험은 열쇠고리를 **건드리지 않는다**(파일 경로만 탄다). 시험이 만든 항목이
    // 남으면, 빌드가 바뀔 때마다 macOS 가 사용자에게 「kasaterm 이 비밀 정보를 쓰려
    // 합니다」 창을 띄운다 — 2026-08-15 에 실제로 그 창이 반복해 떴다.
    //
    // 읽기는 **Security 프레임워크가 아니라 `security` CLI 로 간다.** 금고 항목의
    // 주인은 claude 고, claude 는 `security` CLI 로 항목을 만든다 — 그 항목을 우리
    // 실행파일이 프레임워크로 직접 열면 「만든 프로그램이 아님」이라 macOS 가 매번
    // 승인 창을 띄운다. 폴러가 주기마다 전 금고를 읽으니 앱을 켤 때마다 계정 수만큼
    // 창이 쏟아졌다(2026-08-16 실사용 보고). 같은 CLI 로 읽으면 조용하다 — 쓰기가
    // 이미 이 길을 쓰는 이유이기도 하다.
    #[cfg(all(target_os = "macos", not(test)))]
    {
        let out = crate::proc::command("security")
            .args([
                "find-generic-password",
                "-w",
                "-s", &service_name(dir),
                "-a", &keychain_account(),
            ])
            .stderr(std::process::Stdio::null())
            .output();
        if let Ok(o) = out {
            if o.status.success() {
                let mut b = o.stdout;
                // `-w` 는 값 뒤에 개행을 붙인다. 그대로 두면 digest 가 우리가 쓴
                // 원문과 어긋나 read_back 이 매 주기 「바뀌었다」로 읽는다.
                while b.last().is_some_and(|c| *c == b'\n' || *c == b'\r') {
                    b.pop();
                }
                if !b.is_empty() {
                    return Some(b);
                }
            }
        }
    }
    // keychain 항목이 없으면 평문 파일이 정본이다(claude 도 같은 순서로 폴백한다).
    std::fs::read(dir?.join(".credentials.json")).ok()
}

/// ⚠️ 시험은 **사용자의 진짜 자리에 못 쓴다.** 임시 폴더 밖이면 무조건 거부한다.
///
/// 2026-08-15 에 전체 시험을 돌렸더니 설정 저장 경로를 타는 시험 하나가 사용자
/// 열쇠고리에 진짜 항목(`_active`)을 만들었고, 그 뒤 빌드가 바뀔 때마다 macOS 가
/// 「kasaterm 이 비밀 정보를 쓰려 합니다」 창을 계속 띄웠다. 시험이 실계정 자리에
/// 손대는 길은 아예 막는다.
#[cfg(test)]
fn writable_in_tests(dir: Option<&Path>) -> bool {
    dir.is_some_and(|d| d.starts_with(std::env::temp_dir()))
}

/// 자격증명 한 벌을 쓴다. 성공하면 true.
///
/// macOS 는 **값만 갱신한다 — ACL 깃발(`-A`)을 들지 않는다.** 전에는 `-A`(모든
/// 프로그램 허용)를 붙였는데, 그 깃발은 **남(claude)이 만든 항목에는 「접근 권한
/// 변경」이라** macOS 가 keychain 암호 창을 띄운다 — 작업대가 claude 의 기본 항목이
/// 된 지금, 전환할 때마다 「security 이(가) … 접근 권한을 변경하고자 합니다」 창이
/// 뜬 원인이 정확히 이것이었다(2026-08-17 사용자 스크린샷). 게다가 `-A` 의 원래
/// 목적(프레임워크 읽기 무음화)은 애초에 무효다 — 파티션 검사가 ACL 보다 먼저라
/// CLI 가 만든 항목은 `-A` 여도 프레임워크 읽기에 창이 뜬다(2026-08-16 실측).
/// 반면 `-U` 값 갱신만은 claude 항목에 조용히 먹는다(같은 값 재기록으로 실측,
/// 2026-08-16·08-17 두 번).
///
/// `security` CLI 를 쓰는 이유: claude 가 만든 항목을 프레임워크로 열면 창이 뜨고,
/// CLI 는 전 조합이 조용해서다(read_credentials 와 같은 근거). 값이 잠깐 명령줄에
/// 실리는데 그건 **같은 사용자만** 볼 수 있다(Orca 도 같은 경로를 쓴다).
pub(crate) fn write_credentials(dir: Option<&Path>, blob: &[u8]) -> bool {
    let ok = write_credentials_inner(dir, blob);
    if ok {
        // 작업대의 내용물이 바뀌었을 수 있다 — 렌더가 보는 캐시를 그 자리에서
        // 버려야 전환이 다음 프레임에 반영된다(TTL 을 기다리면 최대 2초 늦다).
        // 금고에 쓴 경우도 함께 버린다: 헛되이 한 번 더 읽을 뿐이고, 어느 쪽에
        // 썼는지 여기서 가르려 들면 판정이 두 벌이 된다.
        bump_workbench_generation();
    }
    ok
}

fn write_credentials_inner(dir: Option<&Path>, blob: &[u8]) -> bool {
    #[cfg(test)]
    if !writable_in_tests(dir) {
        return false;
    }
    #[cfg(all(target_os = "macos", not(test)))]
    {
        let secret = match std::str::from_utf8(blob) {
            Ok(s) => s,
            // 자격증명은 JSON 문자열이다. 아니면 우리가 아는 모양이 아니니 손대지 않는다.
            Err(_) => return false,
        };
        let (svc, acct) = (service_name(dir), keychain_account());
        let add = |update: bool| -> bool {
            let mut c = crate::proc::command("security");
            c.arg("add-generic-password");
            if update {
                c.arg("-U"); // 있으면 갱신
            }
            c.args(["-s", &svc, "-a", &acct, "-w", secret])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|st| st.success())
                .unwrap_or(false)
        };
        if add(true) {
            return true;
        }
        // 갱신이 막히는 경우가 있다 — claude 가 만든 항목은 「claude 만 열 수 있음」으로
        // 잠겨 있어서다. 그때는 지우고 우리 것으로 새로 만든다. 지우기는 암호 창이 안
        // 뜨고(실측), 값은 이미 메모리에 들고 있으며, 이마저 실패하면 아래 파일 폴백이
        // 받아 준다 — 어느 경로로도 로그인을 잃지 않는다.
        //
        // ⚠️ 그 폴백이 없는 경우엔 지우지도 않는다. `dir` 이 없으면 쓸 파일 자리가 없어
        // 「지우기는 됐는데 다시 만들기가 실패」하면 그 계정이 통째로 사라진다.
        if dir.is_none() {
            return false;
        }
        let _ = crate::proc::command("security")
            .args(["delete-generic-password", "-s", &svc, "-a", &acct])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        if add(false) {
            return true;
        }
    }
    let Some(dir) = dir else { return false };
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    // 평문 경로일 때만 파일이 생긴다. 권한은 본인만 — 기본 umask 로는 그룹까지
    // 읽히는 자리가 있어서 명시한다.
    if std::fs::write(dir.join(".credentials.json"), blob).is_err() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(
            dir.join(".credentials.json"),
            std::fs::Permissions::from_mode(0o600),
        );
    }
    true
}

#[cfg(all(target_os = "macos", not(test)))]
fn keychain_account() -> String {
    // claude 의 `K7()` — keychain 항목의 account 필드는 로그인 사용자명이다.
    // USER 가 비면(Finder 로 띄운 앱은 그럴 수 있다) 홈 폴더 이름으로 — "unknown"
    // 으로 항목을 만들면 같은 서비스명의 껍데기가 하나 더 생기고, 이름만으로 읽는
    // 쪽이 그걸 집는다(2026-09-03 실측: 08-18 에 생긴 껍데기가 활성 계정 신원을 막았다).
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::env::var_os("HOME").and_then(|h| {
                PathBuf::from(h)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
            })
        })
        .unwrap_or_else(|| "unknown".to_string())
}

/// 작업대 지문이 말하는 **활성 계정 id**. 지문이 없으면(작업대를 한 번도 우리가
/// 채운 적 없음) None. `claude auth status` 프로브처럼 「이 계정을 어느 자리로
/// 물어야 하나」를 정할 때 쓴다 — 활성 계정은 금고가 아니라 작업대가 정본이다.
pub(crate) fn workbench_account() -> Option<String> {
    read_stamp_in(&active_dir()?).map(|(a, _)| a).filter(|a| !a.is_empty())
}

pub(crate) fn recover_workbench_account(accounts: &[crate::socket::ClaudeAccount]) -> Option<String> {
    let _mutation = credential_mutation();
    let stamp_home = active_dir()?;
    recover_workbench_account_in(
        &stamp_home,
        workbench_store().as_deref(),
        &accounts.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
        crate::socket::claude_account_dir,
    )
}

fn recover_workbench_account_in(
    stamp_home: &Path,
    store: Option<&Path>,
    accounts: &[&str],
    vault_dir_of: impl Fn(&str) -> Option<PathBuf>,
) -> Option<String> {
    let current = read_credentials(store)?;
    if let Some((id, _)) = read_stamp_in(stamp_home) {
        if !id.is_empty() && accounts.contains(&id.as_str()) {
            return Some(id);
        }
    }
    // Shared tokens prove ownership without trusting the shared profile cache or refreshing a slot.
    let mut matches = accounts.iter().filter(|id| !id.is_empty()).filter_map(|id| {
        let vault = vault_dir_of(id).and_then(|dir| read_credentials(Some(&dir)))?;
        credentials_match(&current, &vault).then(|| (id.to_string(), digest_of(&vault)))
    });
    let (id, vault_digest) = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    // A different workbench digest must remain pending so read-back saves refreshed tokens before switching away.
    write_stamp_with_identity_in(stamp_home, &id, &vault_digest, blob_identity_key(&current).as_ref());
    bump_workbench_generation();
    Some(id)
}

fn credentials_match(a: &[u8], b: &[u8]) -> bool {
    let (Ok(a), Ok(b)) = (serde_json::from_slice::<serde_json::Value>(a), serde_json::from_slice::<serde_json::Value>(b)) else {
        return false;
    };
    ["accessToken", "refreshToken"].iter().any(|key| {
        let token = |v: &serde_json::Value| v.get("claudeAiOauth").unwrap_or(v).get(*key)
            .and_then(|t| t.as_str()).filter(|t| !t.is_empty()).map(str::to_owned);
        token(&a).is_some_and(|value| token(&b).as_ref() == Some(&value))
    })
}

/// 그 계정을 **지금 실제로 읽어야 하는 자리**. 활성 계정은 작업대(도는 pane 들이 보는
/// 자리), 나머지는 각자 금고다.
///
/// 사용량 폴러가 이걸 써야 하는 이유가 결정적이다: 폴러는 만료된 토큰을 만나면 그
/// 슬롯 토큰을 **직접 갱신한다**(`refresh_claude_token`). refresh
/// token 은 1회용이라, 활성 계정을 금고에서 갱신해 버리면 같은 계정을 작업대에서 쓰는
/// 도는 pane 들의 토큰이 그 순간 죽은 값이 된다 — 세션 전부가 로그아웃된다. 살아 있는
/// 신원 하나당 자리도 하나여야 한다.
/// 반환의 빈 경로(`PathBuf::new()`)가 곧 작업대다 — 문자열로 펴면 `""` 가 되고,
/// 그 빈 문자열이 이 앱 전체에서 「기본 자리」를 뜻하는 관례라(프록시 쿼리·env·
/// 사용량 표 키) 호출부는 아무것도 바꿀 것이 없다.
pub(crate) fn runtime_dir_for(account_id: &str, _active_account: &str) -> Option<PathBuf> {
    if !account_id.is_empty() {
        if let Some(stamp_home) = active_dir() {
            // 작업대가 정말 이 계정 것으로 채워져 있을 때만. 아직 못 채웠으면 금고가
            // 정본이고, 그때는 pane 도 금고를 보고 있다(shim 폴백과 같은 판정).
            if read_stamp_in(&stamp_home).is_some_and(|(acct, _)| acct == account_id) {
                // ⚠️ 지문이 「이 계정이 작업대에 있다」고 말하면 **작업대 읽기가
                // 실패해도 금고로 떨어지지 않는다.** `read_credentials` 는 macOS 에서
                // `security` 자식 프로세스라 일시 실패가 있고, 그 한 번이 금고 경로를
                // 폴러에 흘리면 사용량 조회 실패 → 금고 갱신 → 활성
                // 계정 금고가 따로 갱신되며 **작업대의 refresh token 이 소비된 죽은
                // 값이 된다**(1회용). 도는 pane 들은 access token 으로 버텨 멀쩡해
                // 보이다가, 재시작하면 새 claude 들이 refresh 를 시도해 전부
                // 로그아웃된다 — 2026-08-18 22:04 재시작에서 실제로 일어났고, 금고
                // (만료 06:20)가 작업대(06:05)보다 새것인 상태가 그 지문이었다.
                // 사용량 한 사이클을 놓치는 것 < 전 세션 로그아웃.
                return Some(PathBuf::new());
            }
        }
    }
    crate::socket::claude_account_dir(account_id)
}

/// 렌더 경로 전용 — `runtime_dir_for` 와 같은 답을 주되 **프로세스를 안 띄운다**.
///
/// 원본은 활성 계정을 물었을 때 `read_credentials` 를 타고, macOS 에서 그건
/// `security` CLI 를 **자식 프로세스로 띄워** 출력을 기다린다(1회 14ms 실측).
/// 상태줄 사용량 게이지가 이 함수를 프레임마다 불러서, pane 여러 개가 동시에
/// 출력해 프레임이 쉼 없이 뜨는 동안 그 14ms 가 매 프레임 메인 스레드를 세웠다
/// — 5초 스택 샘플에서 메인 스레드의 88%(3045/3468 프레임)가 이 한 자리의
/// 자식 프로세스 대기였다(2026-08-18 실측). GPU 패스는 같은 샘플에서 88이다.
/// 학생이 하나일 땐 프레임이 드물어 안 보이고, 여럿이면 상시 렉이 된다.
///
/// 답이 바뀌는 계기는 **작업대의 내용물이 바뀔 때뿐**이고 그 길은 전부
/// `write_credentials` 를 지난다 — 거기서 무효화하므로 전환은 즉시 반영된다.
/// TTL 은 우리를 안 거치고 바뀌는 경우(사용자가 직접 `claude logout`)의
/// 안전벨트다. 같은 이유로 **오래 들고 있어도 되는 값이 아니다** — 자격증명
/// 자체는 캐시하지 않는다(경로만 남긴다).
pub(crate) fn runtime_dir_for_cached(
    account_id: &str,
    active_account: &str,
) -> Option<PathBuf> {
    // 활성 계정이 아니어도 원본은 작업대 지문 파일을 연다(`read_stamp_in`). 상태줄과 계정
    // 메뉴가 계정마다 이걸 프레임마다 불러, 창이 많아 프레임이 잦으면 파일 열기가 메인
    // 스레드 시간의 한 몫이 됐다(2026-10-07 표본). 계정 수만큼만 자라는 맵이라 다 담는다.
    const TTL: std::time::Duration = std::time::Duration::from_secs(2);
    let now = std::time::Instant::now();
    let mut c = runtime_dir_cache().lock().unwrap();
    if let Some((at, gen, hit)) = c.get(account_id) {
        if *gen == workbench_generation() && now.duration_since(*at) < TTL {
            return hit.clone();
        }
    }
    let v = runtime_dir_for(account_id, active_account);
    c.insert(account_id.to_string(), (now, workbench_generation(), v.clone()));
    v
}

type RuntimeDirCache = std::sync::Mutex<
    std::collections::HashMap<String, (std::time::Instant, u64, Option<PathBuf>)>,
>;

fn runtime_dir_cache() -> &'static RuntimeDirCache {
    static C: std::sync::OnceLock<RuntimeDirCache> = std::sync::OnceLock::new();
    C.get_or_init(Default::default)
}

/// 작업대가 바뀔 때마다 오른다. 캐시는 이 값이 그대로일 때만 유효하다 — 시각
/// 비교만으로는 전환 직후 최대 TTL 만큼 옛 답이 남는다.
fn workbench_generation() -> u64 {
    WORKBENCH_GEN.load(std::sync::atomic::Ordering::Relaxed)
}

static WORKBENCH_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 작업대의 내용물을 바꿨다고 알린다. `write_credentials` 가 성공했을 때 부른다.
fn bump_workbench_generation() {
    WORKBENCH_GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// 전환 한 판. 반환은 「무엇이 실제로 일어났나」.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SwapOutcome {
    /// 작업대가 이미 그 계정 것이라 아무것도 안 했다.
    AlreadyActive,
    /// 갈아 끼웠다 — 도는 claude 는 다음 요청부터 이 계정이다.
    Swapped,
    /// 그 계정 금고가 비어 있다(로그인한 적 없거나 슬롯이 지워졌다).
    VaultEmpty,
    /// 쓰기 실패 — keychain 이 막혔거나 경로를 못 만들었다.
    WriteFailed,
    IdentityUnverified,
    PreservationPending,
    WorkbenchChanged,
}

impl SwapOutcome {
    pub(crate) fn failure_code(self) -> &'static str {
        match self {
            Self::WorkbenchChanged => "account_sync_workbench_changed",
            Self::PreservationPending => "account_sync_preservation_pending",
            Self::IdentityUnverified => "account_sync_identity_unverified",
            Self::WriteFailed => "account_sync_storage_failed",
            _ => "account_switch_failed",
        }
    }

    pub(crate) fn failure_message(self) -> &'static str {
        match self {
            Self::WorkbenchChanged => "현재 로그인이 다른 계정으로 바뀌어 보존했어요. 계정 화면에서 현재 로그인 계정을 추가·로그인한 뒤 그 계정을 먼저 선택해 주세요",
            Self::PreservationPending => "현재 로그인을 안전하게 보관하기 전에는 전환하지 않아요. 신원을 다시 확인 중이니 잠시 뒤 같은 계정을 다시 선택해 주세요",
            Self::IdentityUnverified => "계정 신원을 다시 확인 중이에요. 잠시 뒤 같은 계정을 다시 선택해 주세요",
            Self::WriteFailed => "계정 저장소 접근·저장을 확인하지 못했어요. 저장소 접근을 확인한 뒤 다시 선택해 주세요",
            _ => "선택한 계정에 로그인이 없어요. 계정 화면에서 로그인한 뒤 다시 선택해 주세요",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadBack {
    Unchanged,
    Saved,
    Deferred(SwapOutcome),
}

/// 도는 세션이 갱신해 둔 자격증명을 **금고로 되받는다**. 전환 직전과 주기적으로
/// 부른다. 되받지 않으면 금고의 refresh token 이 이미 쓴 값으로 굳어, 다음에 그
/// 계정을 꺼낼 때 로그아웃된 채로 꺼내진다.
///
/// 반환: 되받았으면 true.
pub(crate) fn read_back(vault_dir_of: impl Fn(&str) -> Option<PathBuf>) -> bool {
    let _mutation = credential_mutation();
    let Some(stamp_home) = active_dir() else { return false };
    read_back_in(&stamp_home, workbench_store().as_deref(), vault_dir_of)
}

fn read_back_in(
    stamp_home: &Path,
    store: Option<&Path>,
    vault_dir_of: impl Fn(&str) -> Option<PathBuf>,
) -> bool {
    read_back_verified_in(stamp_home, store, vault_dir_of, blob_identity_key) == ReadBack::Saved
}

fn read_back_verified_in(
    stamp_home: &Path,
    store: Option<&Path>,
    vault_dir_of: impl Fn(&str) -> Option<PathBuf>,
    identity_of: impl Fn(&[u8]) -> Option<IdentityKey>,
) -> ReadBack {
    let Some((stamped_account, stamped_digest)) = read_stamp_in(stamp_home) else {
        // 아직 한 번도 우리가 쓴 적 없는 작업대 — 되받을 정본이 없다.
        return ReadBack::Unchanged;
    };
    if stamped_account.is_empty() {
        return ReadBack::Unchanged;
    }
    let defer = ReadBack::Deferred(SwapOutcome::PreservationPending);
    let Some(now) = read_credentials(store) else { return defer };
    let digest = digest_of(&now);
    let Some(vault) = vault_dir_of(&stamped_account) else { return defer };
    let Some(previous) = read_credentials(Some(&vault)) else { return defer };
    let previous_digest = digest_of(&previous);
    if digest == previous_digest {
        if read_stamp_in(stamp_home).as_ref() != Some(&(stamped_account.clone(), stamped_digest.clone()))
            || read_credentials(store).is_none_or(|current| digest_of(&current) != digest)
            || read_credentials(Some(&vault)).is_none_or(|current| digest_of(&current) != digest) { return defer; }
        if digest == stamped_digest && read_stamp_identity_in(stamp_home).is_none()
            && read_credentials(store).is_some_and(|current| digest_of(&current) == digest)
            && read_stamp_in(stamp_home).as_ref() == Some(&(stamped_account.clone(), stamped_digest.clone())) {
            if let Some(identity) = identity_of(&now) {
                write_stamp_with_identity_in(stamp_home, &stamped_account, &digest, Some(&identity));
            }
        }
        return ReadBack::Unchanged;
    }
    let Some(identity) = identity_of(&now) else { return defer };
    let stamped_identity = read_stamp_identity_in(stamp_home);
    let previous_identity = identity_of(&previous)
        .or_else(|| (previous_digest == stamped_digest).then(|| stamped_identity.clone()).flatten());
    let expected = stamped_identity.as_ref().or(previous_identity.as_ref());
    // An outside /login can replace the workbench with another account, not a token refresh.
    if expected.is_some_and(|expected| expected != &identity)
        || previous_identity.as_ref().is_some_and(|previous| previous != &identity) {
        return ReadBack::Deferred(SwapOutcome::WorkbenchChanged);
    }
    if expected.is_none() || previous_identity.is_none() {
        return defer;
    }
    if read_stamp_in(stamp_home).as_ref() != Some(&(stamped_account.clone(), stamped_digest))
        || read_credentials(store).is_none_or(|current| digest_of(&current) != digest)
        || read_credentials(Some(&vault)).is_none_or(|current| digest_of(&current) != previous_digest) {
        return defer;
    }
    if is_newer(&now, &previous) {
        return ReadBack::Unchanged;
    }
    if !write_credentials(Some(&vault), &now)
        || read_credentials(Some(&vault)).as_deref() != Some(now.as_slice()) {
        return ReadBack::Deferred(SwapOutcome::WriteFailed);
    }
    write_stamp_with_identity_in(stamp_home, &stamped_account, &digest, Some(&identity));
    ReadBack::Saved
}

/// 앱이 뜰 때·shim 을 다시 깔 때 작업대를 활성 계정으로 맞추고, pane 이 가리킬 경로를
/// 돌려준다. `None` 이면 작업대를 못 채운 것이라 부르는 쪽이 **금고를 직접 가리키는**
/// 옛 방식으로 폴백해야 한다 — 로그인 안 된 자리를 가리키면 pane 이 로그인 화면으로
/// 뜬다(그게 제일 나쁜 결과다).
pub(crate) fn ensure_active(
    account_id: &str,
    vault_dir_of: impl Fn(&str) -> Option<PathBuf>,
) -> Option<PathBuf> {
    if account_id.is_empty() {
        return Some(PathBuf::new());
    }
    let _mutation = credential_mutation();
    let Some(stamp_home) = active_dir() else { return None };
    let outcome = swap_active_with_identity_in(&stamp_home, workbench_store().as_deref(), account_id,
        vault_dir_of, None, blob_identity_key, false);
    match outcome {
        // 빈 경로 = 작업대(기본 자리). shim 은 이 경우 env 를 아예 안 붙인다.
        SwapOutcome::Swapped | SwapOutcome::AlreadyActive => Some(PathBuf::new()),
        SwapOutcome::VaultEmpty | SwapOutcome::WriteFailed | SwapOutcome::IdentityUnverified | SwapOutcome::PreservationPending | SwapOutcome::WorkbenchChanged
            if workbench_account().as_deref() == Some(account_id) => Some(PathBuf::new()),
        SwapOutcome::VaultEmpty | SwapOutcome::WriteFailed | SwapOutcome::IdentityUnverified | SwapOutcome::PreservationPending | SwapOutcome::WorkbenchChanged => None,
    }
}

/// 금고 → 작업대. 이것 하나가 「재시작 없는 전환」의 본체다.
pub(crate) fn swap_active(
    account_id: &str,
    vault_dir_of: impl Fn(&str) -> Option<PathBuf>,
) -> SwapOutcome {
    let _mutation = credential_mutation();
    let Some(stamp_home) = active_dir() else { return SwapOutcome::WriteFailed };
    swap_active_in(&stamp_home, workbench_store().as_deref(), account_id, vault_dir_of)
}

fn verified_blob_identity(blob: &[u8]) -> Option<kasa_mcp::VerifiedClaudeIdentity> {
    let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_millis();
    let token = live_access_token_at(blob, now_ms)?;
    kasa_mcp::verified_claude_identity_for_token(&token)
}

fn live_access_token_at(blob: &[u8], now_ms: u128) -> Option<String> {
    if expires_at(blob).is_none_or(|expires| expires <= 0 || expires as u128 <= now_ms) { return None; }
    let value: serde_json::Value = serde_json::from_slice(blob).ok()?;
    let token = value.pointer("/claudeAiOauth/accessToken")
        .or_else(|| value.get("accessToken"))?.as_str()?;
    (!token.is_empty()).then(|| token.to_string())
}

pub(crate) fn verified_workbench_identity() -> Option<kasa_mcp::VerifiedClaudeIdentity> {
    if !account_sync_credentials_allowed() {
        return None;
    }
    let _mutation = credential_mutation();
    verified_blob_identity(&read_credentials(workbench_store().as_deref())?)
}

pub(crate) fn swap_active_verified(
    account_id: &str,
    vault_dir_of: impl Fn(&str) -> Option<PathBuf>,
    expected: &crate::session::AccountIdentity,
) -> SwapOutcome {
    if !account_sync_credentials_allowed() {
        return SwapOutcome::IdentityUnverified;
    }
    let _mutation = credential_mutation();
    let Some(stamp_home) = active_dir() else { return SwapOutcome::WriteFailed };
    swap_active_checked_in(&stamp_home, workbench_store().as_deref(), account_id, vault_dir_of,
        Some(&|blob| verified_blob_identity(blob).is_some_and(|actual| expected.matches_verified(actual))))
}

fn account_sync_credentials_allowed() -> bool {
    !cfg!(test) && !crate::version::isolated_environment(|key| std::env::var_os(key).is_some())
        && !["KASATERM_DEVICE_FILE", "KASATERM_MACHINES_FILE", "KASATERM_WINDOW_SIZE", "KASATERM_WINDOW_POS"]
            .iter().any(|key| std::env::var_os(key).is_some())
}

fn swap_active_in(
    stamp_home: &Path,
    store: Option<&Path>,
    account_id: &str,
    vault_dir_of: impl Fn(&str) -> Option<PathBuf>,
) -> SwapOutcome {
    swap_active_checked_in(stamp_home, store, account_id, vault_dir_of, None)
}

fn swap_active_checked_in(
    stamp_home: &Path,
    store: Option<&Path>,
    account_id: &str,
    vault_dir_of: impl Fn(&str) -> Option<PathBuf>,
    verify: Option<&dyn Fn(&[u8]) -> bool>,
) -> SwapOutcome {
    swap_active_with_identity_in(stamp_home, store, account_id, vault_dir_of, verify, blob_identity_key, true)
}

fn swap_active_with_identity_in(
    stamp_home: &Path,
    store: Option<&Path>,
    account_id: &str,
    vault_dir_of: impl Fn(&str) -> Option<PathBuf>,
    verify: Option<&dyn Fn(&[u8]) -> bool>,
    identity_of: impl Fn(&[u8]) -> Option<IdentityKey>,
    explicit: bool,
) -> SwapOutcome {
    if account_id.is_empty() {
        return SwapOutcome::VaultEmpty;
    }
    let accepts = |blob: &[u8]| verify.is_none_or(|verify| verify(blob));
    if verify.is_some() {
        let target = vault_dir_of(account_id);
        let candidate = read_credentials(target.as_deref());
        if !candidate.as_deref().is_some_and(&accepts) {
            return SwapOutcome::IdentityUnverified;
        }
        // Reapplying may retain a newer workbench, so proving the vault alone is insufficient.
        if read_stamp_in(stamp_home).is_some_and(|(account, _)| account == account_id) {
            let current = read_credentials(store);
            if !current.as_deref().is_some_and(&accepts) {
                if let (Some(target), Some(current)) = (candidate.as_deref().and_then(&identity_of), current.as_deref().and_then(&identity_of)) {
                    if target != current { return SwapOutcome::WorkbenchChanged; }
                }
                return SwapOutcome::IdentityUnverified;
            }
        }
    }
    let workbench_before = read_credentials(store).map(|blob| digest_of(&blob));
    // 지문이 아직 없다 = 작업대를 한 번도 우리 것으로 채운 적이 없다. 기본 자리에
    // 있던 원래 로그인을 먼저 떠 둔다 — 등록 안 된 계정이었으면 이 백업이 유일한
    // 사본이 된다. 백업 금고가 이미 차 있으면 안 덮는다(첫 백업이 원본이다).
    if read_stamp_in(stamp_home).is_none_or(|(account, _)| account.is_empty()) {
        if let Some(original) = read_credentials(store) {
            let backup = vault_dir_of(DEFAULT_BACKUP_SLOT);
            let Some(backup) = backup else { return SwapOutcome::PreservationPending };
            match read_credentials(Some(&backup)) {
                Some(saved) if saved == original => {}
                Some(_) => {
                    if !explicit { return SwapOutcome::PreservationPending; }
                    let Some(target) = vault_dir_of(account_id) else { return SwapOutcome::PreservationPending };
                    if let ReadBack::Deferred(reason) = preserve_in_selected_slot(store, &target, &identity_of, SwapOutcome::PreservationPending) {
                        return reason;
                    }
                }
                None if write_credentials(Some(&backup), &original)
                    && read_credentials(Some(&backup)).is_some_and(|saved| saved == original) => {}
                None => return SwapOutcome::WriteFailed,
            }
        }
    }
    // 먼저 되받는다 — 지금 작업대에 있는 것이 떠나는 계정의 최신 토큰일 수 있다.
    if let ReadBack::Deferred(reason) = read_back_verified_in(stamp_home, store, &vault_dir_of, &identity_of) {
        if !explicit { return reason; }
        let Some(target) = vault_dir_of(account_id) else { return reason };
        if let ReadBack::Deferred(reason) = preserve_in_selected_slot(store, &target, &identity_of, reason) {
            return reason;
        }
    }
    let vault = vault_dir_of(account_id);
    let Some(blob) = read_credentials(vault.as_deref()) else {
        return SwapOutcome::VaultEmpty;
    };
    if !accepts(&blob) {
        return SwapOutcome::IdentityUnverified;
    }
    // 만료 시각이 없거나 0 이면 로그인 안 된 껍데기다(실측: acct-1 금고가
    // `expiresAt: 0` 으로 저장돼 있었다). 이걸 작업대에 덮으면 「전환했더니
    // 로그아웃」이 된다 — 전환을 거부하고 빈 금고 취급하면, 재시작 폴백이 그
    // 계정의 로그인 화면을 자연스럽게 띄우고 /login 후 read_back 이 금고를 채운다.
    if !blob_alive(&blob) {
        eprintln!("[account] {account_id} 금고가 로그인 안 된 껍데기(만료 없음) — 전환 거부");
        return SwapOutcome::VaultEmpty;
    }
    let digest = digest_of(&blob);
    if read_stamp_in(stamp_home).is_some_and(|(a, _)| a == account_id)
        && read_credentials(store).is_some_and(|cur| digest_of(&cur) == digest && accepts(&cur))
    {
        write_stamp_with_identity_in(stamp_home, account_id, &digest, identity_of(&blob).as_ref());
        return SwapOutcome::AlreadyActive;
    }
    // 같은 계정을 다시 적용하는데 작업대 쪽이 더 새것이면 덮지 않는다 — read_back
    // 이 어긋난 상태(지문 유실·일시 실패)에서 금고의 옛 blob 로 최신 작업대를
    // 밀면 refresh token 사슬이 끊겨 로그아웃된다. 다른 계정으로의 명시적 전환은
    // 만료 비교가 무의미하므로(계정이 다르면 시각이 달라도 당연) 제외.
    if read_stamp_in(stamp_home).is_some_and(|(a, _)| a == account_id) {
        match read_credentials(store) {
            Some(cur) => {
                if !accepts(&cur) { return SwapOutcome::IdentityUnverified; }
                let same_identity = match (identity_of(&blob), identity_of(&cur)) {
                    (Some(target), Some(current)) => target == current,
                    _ if verify.is_none() && read_stamp_in(stamp_home).is_some_and(|(_, stamped)| stamped == digest_of(&cur)) => {
                        // Startup may precede profile probing; retaining our exact bytes grants nothing new.
                        return SwapOutcome::AlreadyActive;
                    }
                    _ => return SwapOutcome::IdentityUnverified,
                };
                if !same_identity && !explicit { return SwapOutcome::IdentityUnverified; }
                if same_identity && (is_newer(&blob, &cur) || expires_at(&blob).zip(expires_at(&cur)).is_some_and(|(a, b)| a == b)) {
                    eprintln!(
                        "[account] {account_id} 재적용 — 작업대가 금고보다 새것이라 덮지 않음"
                    );
                    return SwapOutcome::AlreadyActive;
                }
            }
            // 같은 계정 재적용인데 작업대를 못 읽었다 — 정말 비었는지 `security`
            // 일시 실패인지 여기선 구분할 수 없다. 덮으면 일시 실패 쪽에서 최신
            // 작업대가 옛 금고로 밀려 로그아웃이므로 이번 사이클은 그냥 넘어간다.
            // 폴러가 매 사이클 다시 부르니 진짜 비어 있어도 복구 기회는 계속 있고,
            // 정말 비어 있는 경우는 pane 의 /login 이 정도(正道)다.
            None => {
                eprintln!("[account] {account_id} 재적용 보류 — 작업대 읽기 실패(일시일 수 있음)");
                return SwapOutcome::WriteFailed;
            }
        }
    }
    if read_credentials(store).map(|current| digest_of(&current)) != workbench_before {
        return SwapOutcome::PreservationPending;
    }
    if !write_credentials(store, &blob) {
        return SwapOutcome::WriteFailed;
    }
    // Ownership must follow a successful write even if the final proof read is unavailable.
    write_stamp_with_identity_in(stamp_home, account_id, &digest, identity_of(&blob).as_ref());
    if verify.is_some() && !read_credentials(store).as_deref().is_some_and(&accepts) {
        return SwapOutcome::IdentityUnverified;
    }
    SwapOutcome::Swapped
}

fn preserve_in_selected_slot(
    store: Option<&Path>,
    target: &Path,
    identity_of: impl Fn(&[u8]) -> Option<IdentityKey>,
    reason: SwapOutcome,
) -> ReadBack {
    preserve_in_selected_slot_with(store, target, identity_of, reason, |target, blob| write_credentials(Some(target), blob))
}

fn preserve_in_selected_slot_with(
    store: Option<&Path>,
    target: &Path,
    identity_of: impl Fn(&[u8]) -> Option<IdentityKey>,
    reason: SwapOutcome,
    write: impl Fn(&Path, &[u8]) -> bool,
) -> ReadBack {
    let deferred = ReadBack::Deferred(reason);
    let Some(current) = read_credentials(store) else { return deferred };
    let Some(previous) = read_credentials(Some(target)) else { return deferred };
    let (Some(current_identity), Some(target_identity)) = (identity_of(&current), identity_of(&previous)) else { return deferred };
    if current_identity != target_identity { return deferred; }
    if read_credentials(store).as_deref() != Some(current.as_slice())
        || read_credentials(Some(target)).as_deref() != Some(previous.as_slice()) { return deferred; }
    // Only an explicitly selected, proven same-account slot can preserve an unowned workbench.
    if previous == current || is_newer(&current, &previous) { return ReadBack::Unchanged; }
    if !write(target, &current)
        || read_credentials(Some(target)).as_deref() != Some(current.as_slice()) {
        return ReadBack::Deferred(SwapOutcome::WriteFailed);
    }
    ReadBack::Saved
}

/// 전환 뒤 `~/.claude.json` 의 `oauthAccount` 캐시를 새 계정 것으로 갈아 끼운다.
/// `/status` 의 Email/Organization 은 토큰이 아니라 **이 캐시**를 보여주므로(파일만
/// 바꿔도 도는 pane 의 /status 가 즉시 따라오는 것을 실측), 저장소만 바꾸면 과금은
/// 새 계정인데 /status 는 옛말을 한다 — 사용자의 첫 신고가 정확히 그 증상이었다.
///
/// 신원은 로컬 kasa-mcp 의 `/claude-identity` 가 금고 토큰으로 프로필 API 를 물어
/// 만들어 준다(oauthAccount 와 같은 모양). 네트워크가 없으면 캐시를 안 바꾼다 —
/// 표시가 낡는 것이 틀린 값을 지어내는 것보다 낫다. GUI 를 막지 않게 스레드로 돈다.
pub(crate) fn adopt_oauth_account_cache(port: String, vault_dir: Option<PathBuf>) {
    std::thread::spawn(move || {
        let dir = vault_dir.map_or(String::new(), |p| p.to_string_lossy().into_owned());
        let enc: String = dir
            .bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'/' | b'~' => {
                    (b as char).to_string()
                }
                _ => format!("%{b:02X}"),
            })
            .collect();
        let url = format!("http://127.0.0.1:{port}/claude-identity?dir={enc}");
        // 부팅 직후에도 불린다 — 신원 서버가 아직 안 떠 있을 수 있어 몇 번 되묻는다.
        // 끝내 못 물으면 캐시를 안 바꾼다(표시가 낡는 쪽이 지어내는 쪽보다 낫다).
        let mut tries = 0;
        let v = loop {
            tries += 1;
            let out =
                crate::proc::command("curl").args(["-s", "--max-time", "8", &url]).output();
            if let Ok(o) = out {
                if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&o.stdout) {
                    if v.get("account").is_some_and(|a| a.is_object()) {
                        break v;
                    }
                }
            }
            if tries >= 5 {
                return;
            }
            std::thread::sleep(std::time::Duration::from_secs(3));
        };
        let Some(account) = v.get("account").filter(|a| a.is_object()) else { return };
        if account.pointer("/emailAddress").and_then(|e| e.as_str()).is_none() {
            return;
        }
        let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))
        else {
            return;
        };
        let path = PathBuf::from(home).join(".claude.json");
        // 이 파일은 모든 claude 세션이 함께 쓴다 — 읽고-고쳐-쓰는 사이에 남이
        // 쓰면 그 기록을 지운다(mcpcol 의 set_claude_mcp_enabled 와 같은 방어).
        // 쓰기 직전 mtime 을 재확인하고, 그 사이 바뀌었으면 포기한다 — 캐시가
        // 낡는 쪽이 남의 세션 기록을 지우는 쪽보다 낫다.
        let Ok(before) = std::fs::metadata(&path).and_then(|m| m.modified()) else { return };
        let Ok(text) = std::fs::read_to_string(&path) else { return };
        let Ok(mut root) = serde_json::from_str::<serde_json::Value>(&text) else { return };
        let Some(obj) = root.as_object_mut() else { return };
        if obj.get("oauthAccount") == Some(account) {
            return;
        }
        obj.insert("oauthAccount".to_string(), account.clone());
        if std::fs::metadata(&path).and_then(|m| m.modified()).ok() != Some(before) {
            return;
        }
        let Ok(body) = serde_json::to_string(&root) else { return };
        let _ = std::fs::write(&path, body);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 사용자 keychain 에서 **실제로 관측된** 이름과 맞는지. 이 값이 어긋나면 전환이
    /// 조용히 엉뚱한 자리를 쓰고, 그건 「바꿨는데 안 바뀐다」로만 보인다.
    #[test]
    fn service_name_matches_observed_keychain_entries() {
        let cases = [
            ("/Users/kasa/.config/kasaterm/claude-accounts/acct-1", "77d5ac7d"),
            ("/Users/kasa/.config/kasaterm/claude-accounts/acct-2", "2d12f0b3"),
            ("/Users/kasa/.config/kasaterm/claude-accounts/acct-3", "7c1ffde9"),
            ("/Users/kasa/.config/kasaterm/claude-accounts/acct-4", "876f6731"),
            ("/Users/kasa/.config/kasaterm/claude-accounts/acct-5", "d6fb66c7"),
        ];
        for (dir, want) in cases {
            assert_eq!(
                service_name(Some(Path::new(dir))),
                format!("Claude Code-credentials-{want}"),
                "{dir}"
            );
        }
    }

    /// 캐시판은 원본과 **같은 답**을 준다. 어긋나면 상태줄이 활성 계정을 못 알아보고
    /// 숫자를 「읽는 중」에 굳히거나, 떠나온 계정의 사용률을 새 계정 이름 옆에 그린다.
    ///
    /// 「자식 프로세스를 안 띄운다」는 여기서 못 잰다 — 시험 빌드는 keychain 경로를
    /// 통째로 건너뛰어 원본도 프로세스를 안 띄운다. 그건 실행 중인 앱의 스택 샘플로
    /// 쟀다(2026-08-18: render_frame 4024 샘플 중 3550 → 0).
    #[test]
    fn cached_runtime_dir_agrees_with_source() {
        // 활성이 아닌 계정도 캐시를 탄다 — 원본이 작업대 지문 파일을 열기 때문이다.
        assert_eq!(
            runtime_dir_for_cached("acct-9", "acct-1"),
            runtime_dir_for("acct-9", "acct-1"),
        );
        // 활성 계정. 두 번 불러도 답이 흔들리지 않는다.
        let a = runtime_dir_for_cached("acct-1", "acct-1");
        let b = runtime_dir_for_cached("acct-1", "acct-1");
        assert_eq!(a, b, "같은 인자에 두 답이 나오면 화면이 프레임마다 떤다");
        assert_eq!(a, runtime_dir_for("acct-1", "acct-1"));
    }

    /// 작업대에 쓰면 캐시를 **그 자리에서** 버린다. TTL 만 믿으면 계정을 바꾼 뒤
    /// 최대 2초 동안 옛 계정 기준으로 그려진다.
    #[test]
    fn workbench_write_bumps_generation() {
        let before = workbench_generation();
        let _ = runtime_dir_for_cached("acct-1", "acct-1");
        bump_workbench_generation();
        assert!(workbench_generation() > before, "세대가 안 오르면 캐시가 안 버려진다");
        assert_eq!(
            runtime_dir_for_cached("acct-1", "acct-1"),
            runtime_dir_for("acct-1", "acct-1"),
        );
    }

    /// ⚠️ **렌더 경로는 원본을 직접 부르면 안 된다.** 부르는 순간 프레임마다
    /// `security` 자식 프로세스가 뜨고, pane 여럿이 동시에 출력하면 메인 스레드가
    /// 거기서 88%를 쓴다 — 2026-08-18 에 잡은 렉이 정확히 이것이었고, 원인이
    /// 렌더 코드가 아니라 계정 코드에 있어서 눈으로는 안 보였다. 리뷰가 놓쳐도
    /// 여기서 걸린다.
    #[test]
    fn render_path_never_calls_uncached_runtime_dir() {
        for (i, line) in include_str!("render.rs").lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            assert!(
                !line.contains("claude_auth::runtime_dir_for("),
                "render.rs:{} 가 캐시 없는 runtime_dir_for 를 부른다 — \
                 프레임마다 security 프로세스가 뜬다. runtime_dir_for_cached 를 써라.\n{}",
                i + 1,
                line.trim()
            );
        }
    }

    /// 기본 로그인은 접미가 없다 — claude 가 env 없이 뜰 때 보는 그 자리다.
    #[test]
    fn default_login_has_no_suffix() {
        assert_eq!(service_name(None), "Claude Code-credentials");
        assert_eq!(service_name(Some(Path::new(""))), "Claude Code-credentials");
    }

    /// 경로가 한 글자만 달라도 자리가 갈린다 — 슬롯 격리의 근거.
    #[test]
    fn different_dirs_never_share_a_slot() {
        let a = service_name(Some(Path::new("/x/acct-1")));
        let b = service_name(Some(Path::new("/x/acct-2")));
        assert_ne!(a, b);
    }

    /// 한글 경로: NFD 로 들어와도 NFC 로 정규화해 claude 와 같은 이름을 만든다.
    #[test]
    fn korean_path_normalizes_to_nfc() {
        use unicode_normalization::UnicodeNormalization;
        let nfc: String = "/Users/kasa/계정/acct-1".nfc().collect();
        let nfd: String = "/Users/kasa/계정/acct-1".nfd().collect();
        assert_ne!(nfc, nfd, "테스트 전제: 두 표기가 실제로 다르다");
        assert_eq!(
            service_name(Some(Path::new(&nfc))),
            service_name(Some(Path::new(&nfd))),
            "같은 폴더인데 자리가 갈리면 전환이 조용히 실패한다"
        );
    }

    /// 만료 시각 비교 — 낡은 값으로 덮지 않는 규칙의 근거.
    #[test]
    fn newer_wins_and_unknown_never_overwrites() {
        let old = br#"{"claudeAiOauth":{"expiresAt":1000}}"#;
        let new = br#"{"claudeAiOauth":{"expiresAt":2000}}"#;
        assert!(is_newer(old, new));
        assert!(!is_newer(new, old));
        // 못 읽는 모양이면 「더 새것」이라 하지 않는다 — 덮기를 막는 쪽으로 기운다.
        assert!(!is_newer(old, b"garbage"));
        assert!(!is_newer(b"garbage", new));
        // 옛 축약형(최상위 expiresAt)도 읽는다.
        assert_eq!(expires_at(br#"{"expiresAt":7}"#), Some(7));
    }

    /// 임시 슬롯 셋(금고 둘·작업대 하나)을 만들고 전환 한 판을 그대로 돌려 본다.
    /// **진짜 저장소를 쓴다** — macOS 면 keychain 항목이 실제로 생겼다 지워진다.
    /// 경로가 임시라 서비스명 해시도 임시라, 사용자 계정 자리와는 절대 안 겹친다.
    struct Slots {
        root: PathBuf,
    }
    impl Slots {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir()
                .join(format!("kasaterm-auth-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            for s in ["vault-a", "vault-b", "active", "workbench"] {
                std::fs::create_dir_all(root.join(s)).unwrap();
            }
            Slots { root }
        }
        fn dir(&self, name: &str) -> PathBuf {
            self.root.join(name)
        }
        /// (지문 폴더, 작업대 저장소) — 실물에서는 (claude-accounts/_active,
        /// claude 기본 자리)에 해당한다. 시험은 둘 다 임시 폴더다.
        fn bench(&self) -> (PathBuf, PathBuf) {
            (self.dir("active"), self.dir("workbench"))
        }
        fn vault_of(&self) -> impl Fn(&str) -> Option<PathBuf> + '_ {
            move |id: &str| Some(self.root.join(format!("vault-{id}")))
        }
    }
    impl Drop for Slots {
        fn drop(&mut self) {
            // 지울 열쇠고리 항목이 없다 — 시험은 파일 경로만 탄다(read/write 의
            // `not(test)` 게이트). 임시 폴더만 치운다.
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn creds(expires: i64, tag: &str) -> Vec<u8> {
        format!(r#"{{"claudeAiOauth":{{"expiresAt":{expires},"tag":"{tag}"}}}}"#).into_bytes()
    }

    fn proof(account: &str) -> IdentityKey {
        IdentityKey { account_uuid: account.into(), organization_uuid: "test-org".into() }
    }

    #[test]
    fn unselected_switch_preserves_workbench_and_named_stamp() {
        let s = Slots::new("unselected");
        let (stamp, bench) = s.bench();
        let original = creds(1000, "external");
        assert!(write_credentials(Some(&bench), &original));
        write_stamp_in(&stamp, "a", &digest_of(&original));
        assert_eq!(swap_active_in(&stamp, Some(&bench), "", s.vault_of()), SwapOutcome::VaultEmpty);
        assert_eq!(read_credentials(Some(&bench)), Some(original));
        assert_eq!(read_stamp_in(&stamp).unwrap().0, "a");
        assert!(!s.dir("vault-").exists());
    }

    #[test]
    fn verified_switch_refuses_changed_credentials_before_any_write() {
        let s = Slots::new("sync-identity-changed");
        let (stamp, bench) = s.bench();
        let previous = creds(1000, "previous");
        let changed = creds(2000, "unexpected-account");
        let proven = creds(2000, "expected-account");
        assert!(write_credentials(Some(&bench), &previous));
        write_stamp_in(&stamp, "b", &digest_of(&previous));
        assert!(write_credentials(Some(&s.dir("vault-a")), &changed));
        assert_eq!(swap_active_checked_in(&stamp, Some(&bench), "a", s.vault_of(),
            Some(&|blob| blob == proven.as_slice())), SwapOutcome::IdentityUnverified);
        assert_eq!(read_credentials(Some(&bench)), Some(previous.clone()));
        assert_eq!(read_credentials(Some(&s.dir("vault-a"))), Some(changed));
        assert_eq!(read_stamp_in(&stamp), Some(("b".into(), digest_of(&previous))));
    }

    #[test]
    fn verified_reselection_checks_the_newer_workbench_not_only_the_vault() {
        let s = Slots::new("sync-newer-workbench");
        let (stamp, bench) = s.bench();
        let proven = creds(1000, "expected-account");
        let changed = creds(2000, "unexpected-account");
        assert!(write_credentials(Some(&s.dir("vault-a")), &proven));
        assert!(write_credentials(Some(&bench), &changed));
        write_stamp_in(&stamp, "a", &digest_of(&proven));
        assert_eq!(swap_active_checked_in(&stamp, Some(&bench), "a", s.vault_of(),
            Some(&|blob| blob == proven.as_slice())), SwapOutcome::IdentityUnverified);
        assert_eq!(read_credentials(Some(&bench)), Some(changed));
        assert_eq!(read_credentials(Some(&s.dir("vault-a"))), Some(proven));
    }

    #[test]
    fn verified_switch_rechecks_the_blob_read_after_readback() {
        let s = Slots::new("sync-between-reads");
        let (stamp, bench) = s.bench();
        let previous = creds(1000, "previous");
        let proven = creds(2000, "expected-account");
        let changed = creds(3000, "unexpected-account");
        assert!(write_credentials(Some(&bench), &previous));
        assert!(write_credentials(Some(&s.dir("vault-b")), &previous));
        write_stamp_in(&stamp, "b", &digest_of(&previous));
        assert!(write_credentials(Some(&s.dir("vault-a")), &proven));
        let calls = std::cell::Cell::new(0);
        let verify = |blob: &[u8]| {
            calls.set(calls.get() + 1);
            if calls.get() == 1 { assert!(write_credentials(Some(&s.dir("vault-a")), &changed)); }
            blob == proven.as_slice()
        };
        assert_eq!(swap_active_checked_in(&stamp, Some(&bench), "a", s.vault_of(), Some(&verify)), SwapOutcome::IdentityUnverified);
        assert_eq!(read_credentials(Some(&bench)), Some(previous));
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn verified_switch_accepts_only_the_proven_blob_and_reselection_stays_checked() {
        let s = Slots::new("sync-verified");
        let (stamp, bench) = s.bench();
        let proven = creds(2000, "expected-account");
        assert!(write_credentials(Some(&s.dir("vault-a")), &proven));
        let verify = |blob: &[u8]| blob == proven.as_slice();
        assert_eq!(swap_active_checked_in(&stamp, Some(&bench), "a", s.vault_of(), Some(&verify)), SwapOutcome::Swapped);
        assert_eq!(read_credentials(Some(&bench)), Some(proven.clone()));
        assert_eq!(swap_active_checked_in(&stamp, Some(&bench), "a", s.vault_of(), Some(&verify)), SwapOutcome::AlreadyActive);
        assert_eq!(swap_active_checked_in(&stamp, Some(&bench), "a", s.vault_of(), Some(&|_| false)), SwapOutcome::IdentityUnverified);
    }

    #[test]
    fn a_failed_postwrite_proof_still_records_the_bytes_that_were_written() {
        let s = Slots::new("sync-postwrite-expiry");
        let (stamp, bench) = s.bench();
        let previous = creds(1000, "b");
        let target = creds(2000, "a");
        assert!(write_credentials(Some(&bench), &previous));
        assert!(write_credentials(Some(&s.dir("vault-b")), &previous));
        assert!(write_credentials(Some(&s.dir("vault-a")), &target));
        write_stamp_in(&stamp, "b", &digest_of(&previous));
        let calls = std::cell::Cell::new(0);
        let verify = |_: &[u8]| { calls.set(calls.get() + 1); calls.get() < 3 };
        assert_eq!(swap_active_checked_in(&stamp, Some(&bench), "a", s.vault_of(), Some(&verify)), SwapOutcome::IdentityUnverified);
        assert_eq!(read_credentials(Some(&bench)), Some(target.clone()));
        assert_eq!(read_stamp_in(&stamp), Some(("a".into(), digest_of(&target))));
        assert!(!read_back_in(&stamp, Some(&bench), s.vault_of()));
        assert_eq!(read_credentials(Some(&s.dir("vault-b"))), Some(previous));
    }

    #[test]
    fn expired_access_tokens_cannot_receive_a_verified_switch_ack() {
        let blob = |expires| format!(r#"{{"claudeAiOauth":{{"accessToken":"fixture-access","expiresAt":{expires}}}}}"#).into_bytes();
        assert_eq!(live_access_token_at(&blob(101), 100).as_deref(), Some("fixture-access"));
        assert!(live_access_token_at(&blob(100), 100).is_none());
        assert!(live_access_token_at(&blob(99), 100).is_none());
        assert!(live_access_token_at(&blob(0), 100).is_none());
        assert!(live_access_token_at(br#"{"claudeAiOauth":{"accessToken":"fixture-access"}}"#, 100).is_none());
    }

    #[test]
    fn external_login_never_copies_account_b_into_account_a_vault() {
        let s = Slots::new("readback-external-login");
        let (stamp, bench) = s.bench();
        let a = creds(1000, "a");
        let b = creds(9000, "b");
        assert!(write_credentials(Some(&s.dir("vault-a")), &a));
        assert!(write_credentials(Some(&bench), &b));
        write_stamp_with_identity_in(&stamp, "a", &digest_of(&a), Some(&proof("a")));
        let identity = |blob: &[u8]| Some(proof(if blob == a.as_slice() { "a" } else { "b" }));
        assert_eq!(read_back_verified_in(&stamp, Some(&bench), s.vault_of(), identity), ReadBack::Deferred(SwapOutcome::WorkbenchChanged));
        assert_eq!(read_credentials(Some(&s.dir("vault-a"))), Some(a.clone()));
        assert_eq!(read_credentials(Some(&bench)), Some(b));
        assert_eq!(read_stamp_in(&stamp), Some(("a".into(), digest_of(&a))));
    }

    #[test]
    fn readback_defers_unknown_proof_then_accepts_verified_same_account_refresh() {
        let s = Slots::new("readback-proof-later");
        let (stamp, bench) = s.bench();
        let old = creds(1000, "old-a");
        let fresh = creds(9000, "new-a");
        assert!(write_credentials(Some(&s.dir("vault-a")), &old));
        assert!(write_credentials(Some(&bench), &fresh));
        write_stamp_with_identity_in(&stamp, "a", &digest_of(&old), Some(&proof("a")));
        assert_eq!(read_back_verified_in(&stamp, Some(&bench), s.vault_of(), |_| None), ReadBack::Deferred(SwapOutcome::PreservationPending));
        assert_eq!(read_credentials(Some(&s.dir("vault-a"))), Some(old.clone()));
        let identity = |blob: &[u8]| (blob == fresh.as_slice()).then(|| proof("a"));
        assert_eq!(read_back_verified_in(&stamp, Some(&bench), s.vault_of(), identity), ReadBack::Saved, "the stamp proves unchanged old vault bytes after the old token proof expires");
        assert_eq!(read_credentials(Some(&s.dir("vault-a"))), Some(fresh));
    }

    #[test]
    fn legacy_stamp_gains_identity_only_while_exact_bytes_are_unchanged() {
        let s = Slots::new("readback-legacy-proof");
        let (stamp, bench) = s.bench();
        let a = creds(1000, "a");
        assert!(write_credentials(Some(&s.dir("vault-a")), &a));
        assert!(write_credentials(Some(&bench), &a));
        write_stamp_in(&stamp, "a", &digest_of(&a));
        assert_eq!(read_back_verified_in(&stamp, Some(&bench), s.vault_of(), |_| Some(proof("a"))), ReadBack::Unchanged);
        assert_eq!(read_stamp_identity_in(&stamp), Some(proof("a")));
        assert_eq!(read_credentials(Some(&s.dir("vault-a"))), Some(a));
    }

    #[test]
    fn readback_rechecks_current_and_vault_after_identity_lookup() {
        for change_vault in [false, true] {
            let s = Slots::new(if change_vault { "readback-vault-race" } else { "readback-current-race" });
            let (stamp, bench) = s.bench();
            let old = creds(1000, "old-a");
            let fresh = creds(9000, "new-a");
            let b = creds(10000, "b");
            assert!(write_credentials(Some(&s.dir("vault-a")), &old));
            assert!(write_credentials(Some(&bench), &fresh));
            write_stamp_with_identity_in(&stamp, "a", &digest_of(&old), Some(&proof("a")));
            let calls = std::cell::Cell::new(0);
            let identity = |_: &[u8]| {
                calls.set(calls.get() + 1);
                if calls.get() == 2 {
                    let target = if change_vault { s.dir("vault-a") } else { bench.clone() };
                    assert!(write_credentials(Some(&target), &b));
                }
                Some(proof("a"))
            };
            assert_eq!(read_back_verified_in(&stamp, Some(&bench), s.vault_of(), identity), ReadBack::Deferred(SwapOutcome::PreservationPending));
            assert_eq!(read_credentials(Some(&s.dir("vault-a"))), Some(if change_vault { b } else { old }));
        }
    }

    #[test]
    fn unpreserved_other_identity_is_not_discarded_even_by_explicit_reselection() {
        let s = Slots::new("reselection-other-identity");
        let (stamp, bench) = s.bench();
        let a = creds(1000, "a");
        let b = creds(9000, "b");
        assert!(write_credentials(Some(&s.dir("vault-a")), &a));
        assert!(write_credentials(Some(&bench), &b));
        write_stamp_with_identity_in(&stamp, "a", &digest_of(&a), Some(&proof("a")));
        let identity = |blob: &[u8]| Some(proof(if blob == a.as_slice() { "a" } else { "b" }));
        assert_eq!(swap_active_with_identity_in(&stamp, Some(&bench), "a", s.vault_of(), None, identity, false), SwapOutcome::WorkbenchChanged);
        assert_eq!(swap_active_with_identity_in(&stamp, Some(&bench), "a", s.vault_of(), None, identity, true), SwapOutcome::WorkbenchChanged);
        assert_eq!(read_credentials(Some(&bench)), Some(b));
        assert_eq!(read_credentials(Some(&s.dir("vault-a"))), Some(a));
    }

    #[test]
    fn unchanged_startup_needs_no_new_proof_but_changed_unknown_bytes_are_deferred() {
        let s = Slots::new("startup-without-proof");
        let (stamp, bench) = s.bench();
        let a = creds(1000, "a");
        assert!(write_credentials(Some(&s.dir("vault-a")), &a));
        assert!(write_credentials(Some(&bench), &a));
        write_stamp_in(&stamp, "a", &digest_of(&a));
        assert_eq!(swap_active_with_identity_in(&stamp, Some(&bench), "a", s.vault_of(), None, |_| None, false), SwapOutcome::AlreadyActive);
        let changed = creds(9000, "unknown");
        assert!(write_credentials(Some(&bench), &changed));
        assert_eq!(swap_active_with_identity_in(&stamp, Some(&bench), "a", s.vault_of(), None, |_| None, true), SwapOutcome::PreservationPending);
        assert_eq!(read_credentials(Some(&bench)), Some(changed));
        assert_eq!(read_credentials(Some(&s.dir("vault-a"))), Some(a));
    }

    #[test]
    fn switching_away_waits_for_proof_and_preserves_the_only_refreshed_credentials() {
        for strict in [false, true] {
            let s = Slots::new(if strict { "pending-readback-peer" } else { "pending-readback-local" });
            let (stamp, bench) = s.bench();
            let old = creds(1000, "a1");
            let fresh = creds(9000, "a2");
            let b = creds(4000, "b");
            assert!(write_credentials(Some(&s.dir("vault-a")), &old));
            assert!(write_credentials(Some(&s.dir("vault-b")), &b));
            assert!(write_credentials(Some(&bench), &fresh));
            write_stamp_with_identity_in(&stamp, "a", &digest_of(&old), Some(&proof("a")));
            let target_guard = |blob: &[u8]| blob == b.as_slice();
            let verify: Option<&dyn Fn(&[u8]) -> bool> = strict.then_some(&target_guard);
            let missing_fresh = |blob: &[u8]| if blob == b.as_slice() { Some(proof("b")) } else { None };
            assert_eq!(swap_active_with_identity_in(&stamp, Some(&bench), "b", s.vault_of(), verify, missing_fresh, true), SwapOutcome::PreservationPending);
            assert_eq!(read_credentials(Some(&bench)), Some(fresh.clone()));
            assert_eq!(read_credentials(Some(&s.dir("vault-a"))), Some(old.clone()));
            assert_eq!(read_credentials(Some(&s.dir("vault-b"))), Some(b.clone()));
            assert_eq!(read_stamp_in(&stamp), Some(("a".into(), digest_of(&old))));
            let proven = |blob: &[u8]| Some(proof(if blob == b.as_slice() { "b" } else { "a" }));
            assert_eq!(swap_active_with_identity_in(&stamp, Some(&bench), "b", s.vault_of(), verify, proven, true), SwapOutcome::Swapped);
            assert_eq!(read_credentials(Some(&s.dir("vault-a"))), Some(fresh));
            assert_eq!(read_credentials(Some(&bench)), Some(b));
        }
    }

    #[test]
    fn explicit_matching_slot_preserves_the_current_login_before_rebinding_ownership() {
        for current_is_newer in [false, true] {
            let s = Slots::new(if current_is_newer { "adopt-current-newer" } else { "adopt-target-newer" });
            let (stamp, bench) = s.bench();
            let a = creds(1000, "a");
            let current = creds(if current_is_newer { 9000 } else { 2000 }, "current-b");
            let target = creds(4000, "target-b");
            assert!(write_credentials(Some(&s.dir("vault-a")), &a));
            assert!(write_credentials(Some(&s.dir("vault-b")), &target));
            assert!(write_credentials(Some(&bench), &current));
            write_stamp_with_identity_in(&stamp, "a", &digest_of(&a), Some(&proof("a")));
            let identity = |blob: &[u8]| Some(proof(if blob == a.as_slice() { "a" } else { "b" }));
            assert_eq!(swap_active_with_identity_in(&stamp, Some(&bench), "b", s.vault_of(), None, identity, false), SwapOutcome::WorkbenchChanged);
            assert_eq!(swap_active_with_identity_in(&stamp, Some(&bench), "b", s.vault_of(), None, identity, true), SwapOutcome::Swapped);
            let newest = if current_is_newer { current } else { target };
            assert_eq!(read_credentials(Some(&bench)), Some(newest.clone()));
            assert_eq!(read_credentials(Some(&s.dir("vault-b"))), Some(newest.clone()));
            assert_eq!(read_credentials(Some(&s.dir("vault-a"))), Some(a));
            assert_eq!(read_stamp_in(&stamp), Some(("b".into(), digest_of(&newest))));
        }
    }

    #[test]
    fn failed_explicit_preservation_leaves_current_and_target_untouched() {
        let s = Slots::new("adopt-storage-failure");
        let (_, bench) = s.bench();
        let current = creds(9000, "current-b");
        let previous = creds(1000, "target-b");
        assert!(write_credentials(Some(&bench), &current));
        assert!(write_credentials(Some(&s.dir("vault-b")), &previous));
        let result = preserve_in_selected_slot_with(Some(&bench), &s.dir("vault-b"), |_| Some(proof("b")),
            SwapOutcome::WorkbenchChanged, |_, _| false);
        assert_eq!(result, ReadBack::Deferred(SwapOutcome::WriteFailed));
        assert_eq!(read_credentials(Some(&bench)), Some(current));
        assert_eq!(read_credentials(Some(&s.dir("vault-b"))), Some(previous));
    }

    #[test]
    fn unstamped_explicit_same_identity_recovery_keeps_the_original_backup_untouched() {
        for blank_stamp in [false, true] {
            let s = Slots::new(if blank_stamp { "blank-stamp-explicit-recovery" } else { "unstamped-explicit-recovery" });
            let (stamp, bench) = s.bench();
            let backup = s.vault_of()(DEFAULT_BACKUP_SLOT).unwrap();
            let a = creds(1000, "saved-a");
            let current = creds(9000, "current-b2");
            let target = creds(4000, "saved-b1");
            assert!(write_credentials(Some(&backup), &a));
            assert!(write_credentials(Some(&s.dir("vault-a")), &a));
            assert!(write_credentials(Some(&s.dir("vault-b")), &target));
            assert!(write_credentials(Some(&bench), &current));
            if blank_stamp { write_stamp_in(&stamp, "", "legacy"); }
            let stamp_before = read_stamp_in(&stamp);
            let missing_current = |blob: &[u8]| (blob == target.as_slice()).then(|| proof("b"));
            assert_eq!(swap_active_with_identity_in(&stamp, Some(&bench), "b", s.vault_of(), None, missing_current, true), SwapOutcome::PreservationPending);
            assert_eq!(read_credentials(Some(&bench)), Some(current.clone()));
            assert_eq!(read_credentials(Some(&s.dir("vault-b"))), Some(target.clone()));
            assert_eq!(read_credentials(Some(&backup)), Some(a.clone()));
            assert_eq!(read_stamp_in(&stamp), stamp_before);
            let known = |blob: &[u8]| Some(proof(if blob == a.as_slice() { "a" } else { "b" }));
            assert_eq!(swap_active_with_identity_in(&stamp, Some(&bench), "b", s.vault_of(), None, known, false), SwapOutcome::PreservationPending);
            assert_eq!(read_credentials(Some(&bench)), Some(current.clone()));
            assert_eq!(swap_active_with_identity_in(&stamp, Some(&bench), "b", s.vault_of(), None, known, true), SwapOutcome::Swapped);
            assert_eq!(read_credentials(Some(&bench)), Some(current.clone()));
            assert_eq!(read_credentials(Some(&s.dir("vault-b"))), Some(current.clone()));
            assert_eq!(read_credentials(Some(&backup)), Some(a.clone()));
            assert_eq!(read_credentials(Some(&s.dir("vault-a"))), Some(a));
            assert_eq!(read_stamp_in(&stamp), Some(("b".into(), digest_of(&current))));
        }
    }

    #[test]
    fn workbench_changed_after_preservation_is_not_overwritten() {
        let s = Slots::new("change-after-readback");
        let (stamp, bench) = s.bench();
        let old = creds(1000, "a1");
        let fresh = creds(9000, "a2");
        let newer = creds(10000, "a3");
        let b = creds(4000, "b");
        assert!(write_credentials(Some(&s.dir("vault-a")), &old));
        assert!(write_credentials(Some(&s.dir("vault-b")), &b));
        assert!(write_credentials(Some(&bench), &fresh));
        write_stamp_with_identity_in(&stamp, "a", &digest_of(&old), Some(&proof("a")));
        let target_reads = std::cell::Cell::new(0);
        let vault_of = |id: &str| {
            if id == "b" {
                target_reads.set(target_reads.get() + 1);
                assert!(write_credentials(Some(&bench), &newer));
            }
            s.vault_of()(id)
        };
        let identity = |blob: &[u8]| Some(proof(if blob == b.as_slice() { "b" } else { "a" }));
        assert_eq!(swap_active_with_identity_in(&stamp, Some(&bench), "b", vault_of, None, identity, true), SwapOutcome::PreservationPending);
        assert_eq!(read_credentials(Some(&bench)), Some(newer));
        assert_eq!(read_credentials(Some(&s.dir("vault-a"))), Some(fresh));
        assert_eq!(target_reads.get(), 1);
    }

    #[test]
    fn failure_messages_explain_safe_recovery_without_requesting_an_unavailable_save_button() {
        assert!(SwapOutcome::PreservationPending.failure_message().contains("잠시 뒤"));
        assert!(SwapOutcome::WorkbenchChanged.failure_message().contains("추가·로그인"));
        assert!(SwapOutcome::WorkbenchChanged.failure_message().contains("먼저 선택"));
        assert_eq!(SwapOutcome::PreservationPending.failure_code(), "account_sync_preservation_pending");
    }

    #[test]
    fn account_sync_test_helpers_never_open_real_credential_stores() {
        assert!(!account_sync_credentials_allowed());
        assert!(verified_workbench_identity().is_none());
        assert!(crate::settings::account_sync_identity("fixture-no-store").is_none());
    }

    #[test]
    fn blank_legacy_stamp_never_reads_back_to_workbench_alias() {
        let s = Slots::new("blank-stamp");
        let (stamp, bench) = s.bench();
        let original = creds(1000, "external");
        assert!(write_credentials(Some(&bench), &original));
        write_stamp_in(&stamp, "", "old");
        assert!(!read_back_in(&stamp, Some(&bench), |_| panic!("empty owner must not resolve a vault")));
        assert_eq!(read_credentials(Some(&bench)), Some(original));
    }

    #[test]
    fn recovery_adopts_unique_token_owner_without_copying_credentials() {
        let s = Slots::new("recover");
        let (stamp, bench) = s.bench();
        let current = br#"{"claudeAiOauth":{"expiresAt":2000,"accessToken":"fake-current","refreshToken":"fake-shared"}}"#;
        let vault = br#"{"claudeAiOauth":{"expiresAt":1000,"accessToken":"fake-old","refreshToken":"fake-shared"}}"#;
        assert!(write_credentials(Some(&bench), current));
        assert!(write_credentials(Some(&s.dir("vault-a")), vault));
        write_stamp_in(&stamp, "", &digest_of(current));
        assert_eq!(recover_workbench_account_in(&stamp, Some(&bench), &["a", "b"], s.vault_of()).as_deref(), Some("a"));
        assert_eq!(read_credentials(Some(&bench)).unwrap(), current);
        assert_eq!(read_credentials(Some(&s.dir("vault-a"))).unwrap(), vault);
        assert_eq!(read_stamp_in(&stamp).unwrap(), ("a".into(), digest_of(vault)));
        assert_eq!(recover_workbench_account_in(&stamp, Some(&bench), &["a", "b"], s.vault_of()).as_deref(), Some("a"));
        assert_eq!(read_stamp_in(&stamp).unwrap().1, digest_of(vault));
        assert!(write_credentials(Some(&s.dir("vault-b")), &creds(1000, "b")));
        let identity = |blob: &[u8]| Some(proof(if blob == current.as_slice() || blob == vault.as_slice() { "a" } else { "b" }));
        assert_eq!(swap_active_with_identity_in(&stamp, Some(&bench), "b", s.vault_of(), None, identity, true), SwapOutcome::Swapped);
        assert_eq!(read_credentials(Some(&s.dir("vault-a"))).unwrap(), current);
    }

    #[test]
    fn recovery_leaves_unknown_or_ambiguous_login_unselected() {
        let s = Slots::new("recover-ambiguous");
        let (stamp, bench) = s.bench();
        let current = br#"{"claudeAiOauth":{"expiresAt":2000,"accessToken":"fake-shared"}}"#;
        assert!(write_credentials(Some(&bench), current));
        assert_eq!(recover_workbench_account_in(&stamp, Some(&bench), &["a", "b"], s.vault_of()), None);
        for id in ["a", "b"] {
            assert!(write_credentials(s.vault_of()(id).as_deref(), current));
        }
        assert_eq!(recover_workbench_account_in(&stamp, Some(&bench), &["a", "b"], s.vault_of()), None);
        assert_eq!(read_credentials(Some(&bench)).unwrap(), current);
        assert!(read_stamp_in(&stamp).is_none());
        assert!(!credentials_match(&creds(1000, "a"), &creds(1000, "a")));
    }

    /// ★2026-08-18 재시작 전-pane 로그아웃의 안전벨트들.
    ///
    /// ① 만료 시각이 없거나 0 인 금고(로그인 안 된 껍데기 — acct-1 실측)로는
    ///    전환하지 않는다. 작업대에 덮으면 「전환했더니 로그아웃」이 된다.
    /// ② 같은 계정 재적용에서 작업대가 금고보다 새것이면 덮지 않는다 — 지문이
    ///    어긋난 상태에서 옛 blob 로 밀면 refresh token 사슬이 끊긴다(1회용).
    #[test]
    fn swap_never_installs_dead_or_older_blob_over_newer_workbench() {
        let s = Slots::new("guard");
        let (stamp, bench) = s.bench();
        // ① 죽은 껍데기 — expiresAt 0.
        assert!(write_credentials(Some(&s.dir("vault-a")), &creds(0, "dead")));
        assert_eq!(
            swap_active_in(&stamp, Some(&bench), "a", s.vault_of()),
            SwapOutcome::VaultEmpty,
            "만료 0 blob 이 작업대에 실리면 그 순간 로그아웃이다"
        );
        assert!(read_credentials(Some(&bench)).is_none(), "작업대는 손대지 않는다");

        // 정상 전환으로 작업대를 채운다.
        assert!(write_credentials(Some(&s.dir("vault-a")), &creds(1000, "old")));
        assert_eq!(
            swap_active_in(&stamp, Some(&bench), "a", s.vault_of()),
            SwapOutcome::Swapped
        );
        // ② 도는 세션이 작업대를 갱신했는데(더 새것) 지문 갱신이 어긋난 상황을
        //    재현: 작업대만 새 값으로 바꾸고 지문은 옛 digest 그대로 둔 채, 금고를
        //    다른 옛 값으로 바꿔 read_back 의 digest 일치 경로를 막는다.
        assert!(write_credentials(Some(&bench), &creds(2000, "fresh")));
        assert!(write_credentials(Some(&s.dir("vault-a")), &creds(1500, "stale")));
        // read_back 은 vault(1500) 가 bench(2000) 보다 옛것이라 bench 를 되받고,
        // 어떤 경로로든 **작업대의 2000 이 1500 으로 내려가면 안 된다**.
        let out = swap_active_in(&stamp, Some(&bench), "a", s.vault_of());
        let bench_now = read_credentials(Some(&bench)).unwrap();
        assert!(
            expires_at(&bench_now).unwrap() >= 2000,
            "작업대가 옛 blob({out:?})로 덮였다 — 로그아웃 경로"
        );
    }

    /// 전환의 본체 — 금고에서 작업대로 옮겨 담기고, 두 번째로 같은 계정을 눌러도
    /// 다시 쓰지 않는다(그 자리에 도는 세션의 최신 토큰이 있을 수 있다).
    #[test]
    fn swap_moves_vault_into_active_and_is_idempotent() {
        let s = Slots::new("swap");
        let (stamp, bench) = s.bench();
        assert!(write_credentials(Some(&s.dir("vault-a")), &creds(1000, "a")));
        assert!(write_credentials(Some(&s.dir("vault-b")), &creds(1000, "b")));

        assert_eq!(
            swap_active_in(&stamp, Some(&bench), "a", s.vault_of()),
            SwapOutcome::Swapped
        );
        assert_eq!(read_credentials(Some(&bench)).unwrap(), creds(1000, "a"));
        // 같은 계정을 또 골라도 덮지 않는다.
        assert_eq!(
            swap_active_in(&stamp, Some(&bench), "a", s.vault_of()),
            SwapOutcome::AlreadyActive
        );
        // 다른 계정으로 갈아 끼우기.
        assert_eq!(
            swap_active_in(&stamp, Some(&bench), "b", s.vault_of()),
            SwapOutcome::Swapped
        );
        assert_eq!(read_credentials(Some(&bench)).unwrap(), creds(1000, "b"));
        // 로그인 없는 슬롯은 작업대를 건드리지 않는다 — 빈 자리를 주면 pane 이
        // 로그인 화면으로 뜬다.
        assert_eq!(
            swap_active_in(&stamp, Some(&bench), "zzz", s.vault_of()),
            SwapOutcome::VaultEmpty
        );
        assert_eq!(read_credentials(Some(&bench)).unwrap(), creds(1000, "b"));
    }

    /// 첫 전환은 기본 자리에 있던 원래 로그인을 백업 금고로 먼저 떠 둔다 — 등록 안
    /// 된 계정이었으면 이 백업이 유일한 사본이다. 두 번째 전환부터는 덮지 않는다.
    #[test]
    fn first_swap_backs_up_the_original_default_login() {
        let s = Slots::new("backup");
        let (stamp, bench) = s.bench();
        // 기본 자리에 kasaterm 이 모르는 로그인이 살고 있었다.
        assert!(write_credentials(Some(&bench), &creds(500, "original")));
        assert!(write_credentials(Some(&s.dir("vault-a")), &creds(1000, "a")));
        assert_eq!(
            swap_active_in(&stamp, Some(&bench), "a", s.vault_of()),
            SwapOutcome::Swapped
        );
        let backup = s.vault_of()(DEFAULT_BACKUP_SLOT).unwrap();
        assert_eq!(read_credentials(Some(&backup)).unwrap(), creds(500, "original"));
        // 이후 전환은 백업을 덮지 않는다 — 첫 백업이 원본이다.
        assert!(write_credentials(Some(&s.dir("vault-b")), &creds(1000, "b")));
        assert_eq!(
            swap_active_in(&stamp, Some(&bench), "b", s.vault_of()),
            SwapOutcome::Swapped
        );
        assert_eq!(read_credentials(Some(&backup)).unwrap(), creds(500, "original"));
    }

    /// **계정이 죽지 않는 이유**: 도는 세션이 작업대에서 토큰을 갱신하면 그 값을
    /// 금고로 되받는다. 안 되받으면 금고에 이미 쓴 refresh token 이 남아, 다음에 그
    /// 계정을 꺼낼 때 로그아웃된 채로 꺼내진다(1회용이라 되돌릴 수 없다).
    #[test]
    fn live_refresh_is_carried_back_into_the_vault() {
        let s = Slots::new("readback");
        let (stamp, bench) = s.bench();
        assert!(write_credentials(Some(&s.dir("vault-a")), &creds(1000, "a")));
        assert_eq!(
            swap_active_in(&stamp, Some(&bench), "a", s.vault_of()),
            SwapOutcome::Swapped
        );

        // 도는 claude 가 갱신했다 — 작업대에만 새 토큰이 있다.
        assert!(write_credentials(Some(&bench), &creds(9000, "a2")));
        assert_eq!(read_back_verified_in(&stamp, Some(&bench), s.vault_of(), |_| Some(proof("a"))), ReadBack::Saved, "같은 공급자 계정이 확인됐을 때 되받아야 한다");
        assert_eq!(read_credentials(Some(&s.dir("vault-a"))).unwrap(), creds(9000, "a2"));
        // 두 번째 호출은 할 일이 없다.
        assert!(!read_back_in(&stamp, Some(&bench), s.vault_of()));

        // 그 뒤 다른 계정으로 갔다가 돌아와도 **갱신된** 토큰이 나온다.
        assert!(write_credentials(Some(&s.dir("vault-b")), &creds(1000, "b")));
        assert_eq!(
            swap_active_in(&stamp, Some(&bench), "b", s.vault_of()),
            SwapOutcome::Swapped
        );
        assert_eq!(
            swap_active_in(&stamp, Some(&bench), "a", s.vault_of()),
            SwapOutcome::Swapped
        );
        assert_eq!(read_credentials(Some(&bench)).unwrap(), creds(9000, "a2"));
    }

    /// 낡은 값으로 금고를 덮지 않는다 — 옛 방식으로 묶인 pane 이 금고를 먼저 갱신한
    /// 경우다. 덮으면 그 계정이 통째로 로그아웃된다.
    #[test]
    fn read_back_never_overwrites_a_newer_vault() {
        let s = Slots::new("newer");
        let (stamp, bench) = s.bench();
        assert!(write_credentials(Some(&s.dir("vault-a")), &creds(1000, "a")));
        assert_eq!(
            swap_active_in(&stamp, Some(&bench), "a", s.vault_of()),
            SwapOutcome::Swapped
        );
        // 금고 쪽이 더 새것이 됐다(옛 pane 이 갱신).
        assert!(write_credentials(Some(&s.dir("vault-a")), &creds(9000, "vault-new")));
        // 작업대에는 그보다 낡은 값이 들어온다.
        assert!(write_credentials(Some(&bench), &creds(2000, "active-old")));
        assert_eq!(
            read_back_verified_in(&stamp, Some(&bench), s.vault_of(), |_| Some(proof("a"))), ReadBack::Unchanged,
            "낡은 값으로 덮으면 안 된다"
        );
        assert_eq!(
            read_credentials(Some(&s.dir("vault-a"))).unwrap(),
            creds(9000, "vault-new")
        );
    }

    /// 파일 백엔드(비-macOS 경로)의 왕복. keychain 이 없는 자리에서도 정본이 산다.
    #[test]
    fn file_backend_round_trips() {
        let tmp = std::env::temp_dir().join(format!(
            "kasaterm-auth-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join(".credentials.json"), b"{\"x\":1}").unwrap();
        assert_eq!(read_credentials(Some(&tmp)).as_deref(), Some(&b"{\"x\":1}"[..]));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
