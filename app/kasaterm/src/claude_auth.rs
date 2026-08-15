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
//! ## 구조 — 금고와 작업대를 가른다
//!
//! - **금고**(vault): 계정마다 하나. `claude-accounts/acct-N` 슬롯. 정본이다.
//! - **작업대**(active): `claude-accounts/_active` 슬롯 하나. pane 의 claude 는 전부
//!   여기를 보고 돈다. 전환 = 금고에서 작업대로 옮겨 담기.
//!
//! 작업대를 따로 두는 이유는 두 가지다. ①사용자의 원래 로그인(kasaterm 밖에서 그냥
//! `claude` 를 칠 때 쓰는 기본 슬롯)을 건드리지 않는다 ②모든 pane 이 **같은 한 자리**를
//! 보므로 갈아 끼우기 한 번이 전부에게 닿는다.
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

/// 작업대 슬롯의 폴더 이름. 계정 id 로는 못 쓰는 이름이라(슬롯 id 는 `acct-N`)
/// 실제 계정과 충돌하지 않는다.
pub(crate) const ACTIVE_SLOT: &str = "_active";

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

/// 작업대 슬롯 경로. `settings.json` 이 있는 폴더 아래라 헤드리스 검증이 스크래치
/// 설정을 가리키면 작업대도 스크래치로 따라간다(계정 폴더와 같은 규칙).
pub(crate) fn active_dir() -> Option<PathBuf> {
    crate::socket::claude_account_dir(ACTIVE_SLOT)
}

/// 지문 파일 — 「우리가 작업대에 마지막으로 써 넣은 것」의 해시와 그때의 계정 id.
/// 이 둘이 있어야 「도는 세션이 갱신한 것」과 「우리가 방금 넣은 것」을 가른다.
fn stamp_path_in(active: &Path) -> PathBuf {
    active.join("active-stamp.json")
}

fn stamp_path() -> Option<PathBuf> {
    Some(stamp_path_in(&active_dir()?))
}

/// 자격증명 한 벌의 **만료 시각**(epoch ms). 어느 쪽이 최신인지 가르는 유일한 단서라
/// 값 전체를 비교하지 않고 이 한 숫자만 꺼낸다 — 토큰은 절대 밖으로 안 나간다.
///
/// 두 자리(금고·작업대)가 잠깐 공존하는 구간이 있고, refresh token 은 1회용이라
/// **오래된 쪽으로 덮으면 그 계정이 로그아웃된다.** 그래서 되받기는 언제나 「더 새것이
/// 이긴다」로 판정한다.
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

fn read_stamp() -> Option<(String, String)> {
    let raw = std::fs::read_to_string(stamp_path()?).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    Some((
        v.get("account")?.as_str()?.to_string(),
        v.get("digest")?.as_str()?.to_string(),
    ))
}

fn write_stamp_in(active: &Path, account: &str, digest: &str) {
    let p = stamp_path_in(active);
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let v = serde_json::json!({ "account": account, "digest": digest });
    let _ = std::fs::write(p, v.to_string());
}

fn write_stamp(account: &str, digest: &str) {
    if let Some(a) = active_dir() {
        write_stamp_in(&a, account, digest);
    }
}

/// 자격증명 한 벌을 읽는다. macOS 는 keychain, 그 밖은 `<dir>/.credentials.json`.
/// 값은 호출부 메모리에만 머문다 — 로그에도 화면에도 내보내지 않는다.
pub(crate) fn read_credentials(dir: Option<&Path>) -> Option<Vec<u8>> {
    #[cfg(target_os = "macos")]
    {
        use security_framework::passwords::get_generic_password;
        if let Ok(b) = get_generic_password(&service_name(dir), &keychain_account()) {
            return Some(b);
        }
    }
    // keychain 항목이 없으면 평문 파일이 정본이다(claude 도 같은 순서로 폴백한다).
    std::fs::read(dir?.join(".credentials.json")).ok()
}

/// 자격증명 한 벌을 쓴다. 성공하면 true.
pub(crate) fn write_credentials(dir: Option<&Path>, blob: &[u8]) -> bool {
    #[cfg(target_os = "macos")]
    {
        use security_framework::passwords::set_generic_password;
        if set_generic_password(&service_name(dir), &keychain_account(), blob).is_ok() {
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

#[cfg(target_os = "macos")]
fn keychain_account() -> String {
    // claude 의 `K7()` — keychain 항목의 account 필드는 로그인 사용자명이다.
    std::env::var("USER").unwrap_or_else(|_| "unknown".to_string())
}

/// 그 계정을 **지금 실제로 읽어야 하는 자리**. 활성 계정은 작업대(도는 pane 들이 보는
/// 자리), 나머지는 각자 금고다.
///
/// 사용량 폴러가 이걸 써야 하는 이유가 결정적이다: 폴러는 만료된 토큰을 만나면 그
/// 슬롯으로 claude 를 한 번 돌려 **갱신을 유발한다**(`refresh_slot_once`). refresh
/// token 은 1회용이라, 활성 계정을 금고에서 갱신해 버리면 같은 계정을 작업대에서 쓰는
/// 도는 pane 들의 토큰이 그 순간 죽은 값이 된다 — 세션 전부가 로그아웃된다. 살아 있는
/// 신원 하나당 자리도 하나여야 한다.
pub(crate) fn runtime_dir_for(account_id: &str, active_account: &str) -> Option<PathBuf> {
    if account_id == active_account {
        if let Some(a) = active_dir() {
            // 작업대가 정말 이 계정 것으로 채워져 있을 때만. 아직 못 채웠으면 금고가
            // 정본이고, 그때는 pane 도 금고를 보고 있다(shim 폴백과 같은 판정).
            if read_stamp().is_some_and(|(acct, _)| acct == account_id)
                && read_credentials(Some(&a)).is_some()
            {
                return Some(a);
            }
        }
    }
    crate::socket::claude_account_dir(account_id)
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
}

/// 도는 세션이 갱신해 둔 자격증명을 **금고로 되받는다**. 전환 직전과 주기적으로
/// 부른다. 되받지 않으면 금고의 refresh token 이 이미 쓴 값으로 굳어, 다음에 그
/// 계정을 꺼낼 때 로그아웃된 채로 꺼내진다.
///
/// 반환: 되받았으면 true.
pub(crate) fn read_back(vault_dir_of: impl Fn(&str) -> Option<PathBuf>) -> bool {
    let Some(active) = active_dir() else { return false };
    read_back_in(&active, vault_dir_of)
}

fn read_back_in(active: &Path, vault_dir_of: impl Fn(&str) -> Option<PathBuf>) -> bool {
    let Some((stamped_account, stamped_digest)) = read_stamp_in(active) else {
        // 아직 한 번도 우리가 쓴 적 없는 작업대 — 되받을 정본이 없다.
        return false;
    };
    let Some(now) = read_credentials(Some(active)) else { return false };
    let digest = digest_of(&now);
    if digest == stamped_digest {
        return false; // 우리가 넣은 그대로다.
    }
    // 달라졌다 = 그 계정으로 도는 세션이 갱신했다. 금고를 그 값으로 맞춘다 —
    // 단 금고 쪽이 더 새것이면 손대지 않는다(옛 슬롯에 묶인 pane 이 금고를 먼저
    // 갱신했을 수 있고, 그 위에 낡은 값을 덮으면 계정이 통째로 로그아웃된다).
    let vault = vault_dir_of(&stamped_account);
    if let Some(cur) = read_credentials(vault.as_deref()) {
        if is_newer(&now, &cur) {
            return false;
        }
    }
    if !write_credentials(vault.as_deref(), &now) {
        return false;
    }
    write_stamp_in(active, &stamped_account, &digest);
    true
}

/// 앱이 뜰 때·shim 을 다시 깔 때 작업대를 활성 계정으로 맞추고, pane 이 가리킬 경로를
/// 돌려준다. `None` 이면 작업대를 못 채운 것이라 부르는 쪽이 **금고를 직접 가리키는**
/// 옛 방식으로 폴백해야 한다 — 로그인 안 된 자리를 가리키면 pane 이 로그인 화면으로
/// 뜬다(그게 제일 나쁜 결과다).
pub(crate) fn ensure_active(
    account_id: &str,
    vault_dir_of: impl Fn(&str) -> Option<PathBuf>,
) -> Option<PathBuf> {
    match swap_active(account_id, vault_dir_of) {
        SwapOutcome::Swapped | SwapOutcome::AlreadyActive => active_dir(),
        SwapOutcome::VaultEmpty | SwapOutcome::WriteFailed => None,
    }
}

/// 금고 → 작업대. 이것 하나가 「재시작 없는 전환」의 본체다.
pub(crate) fn swap_active(
    account_id: &str,
    vault_dir_of: impl Fn(&str) -> Option<PathBuf>,
) -> SwapOutcome {
    let Some(active) = active_dir() else { return SwapOutcome::WriteFailed };
    swap_active_in(&active, account_id, vault_dir_of)
}

fn swap_active_in(
    active: &Path,
    account_id: &str,
    vault_dir_of: impl Fn(&str) -> Option<PathBuf>,
) -> SwapOutcome {
    // 먼저 되받는다 — 지금 작업대에 있는 것이 떠나는 계정의 최신 토큰일 수 있다.
    read_back_in(active, &vault_dir_of);
    let vault = vault_dir_of(account_id);
    let Some(blob) = read_credentials(vault.as_deref()) else {
        return SwapOutcome::VaultEmpty;
    };
    let digest = digest_of(&blob);
    if read_stamp_in(active).is_some_and(|(a, d)| a == account_id && d == digest)
        && read_credentials(Some(active)).is_some_and(|cur| digest_of(&cur) == digest)
    {
        return SwapOutcome::AlreadyActive;
    }
    if !write_credentials(Some(active), &blob) {
        return SwapOutcome::WriteFailed;
    }
    write_stamp_in(active, account_id, &digest);
    SwapOutcome::Swapped
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
            for s in ["vault-a", "vault-b", "active"] {
                std::fs::create_dir_all(root.join(s)).unwrap();
            }
            Slots { root }
        }
        fn dir(&self, name: &str) -> PathBuf {
            self.root.join(name)
        }
        fn vault_of(&self) -> impl Fn(&str) -> Option<PathBuf> + '_ {
            move |id: &str| Some(self.root.join(format!("vault-{id}")))
        }
    }
    impl Drop for Slots {
        fn drop(&mut self) {
            // keychain 에 남은 임시 항목을 지운다 — 시험이 사용자 열쇠고리에
            // 쓰레기를 쌓으면 안 된다.
            #[cfg(target_os = "macos")]
            for s in ["vault-a", "vault-b", "active"] {
                use security_framework::passwords::delete_generic_password;
                let _ = delete_generic_password(
                    &service_name(Some(&self.root.join(s))),
                    &keychain_account(),
                );
            }
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn creds(expires: i64, tag: &str) -> Vec<u8> {
        format!(r#"{{"claudeAiOauth":{{"expiresAt":{expires},"tag":"{tag}"}}}}"#).into_bytes()
    }

    /// 전환의 본체 — 금고에서 작업대로 옮겨 담기고, 두 번째로 같은 계정을 눌러도
    /// 다시 쓰지 않는다(그 자리에 도는 세션의 최신 토큰이 있을 수 있다).
    #[test]
    fn swap_moves_vault_into_active_and_is_idempotent() {
        let s = Slots::new("swap");
        let active = s.dir("active");
        assert!(write_credentials(Some(&s.dir("vault-a")), &creds(1000, "a")));
        assert!(write_credentials(Some(&s.dir("vault-b")), &creds(1000, "b")));

        assert_eq!(swap_active_in(&active, "a", s.vault_of()), SwapOutcome::Swapped);
        assert_eq!(read_credentials(Some(&active)).unwrap(), creds(1000, "a"));
        // 같은 계정을 또 골라도 덮지 않는다.
        assert_eq!(swap_active_in(&active, "a", s.vault_of()), SwapOutcome::AlreadyActive);
        // 다른 계정으로 갈아 끼우기.
        assert_eq!(swap_active_in(&active, "b", s.vault_of()), SwapOutcome::Swapped);
        assert_eq!(read_credentials(Some(&active)).unwrap(), creds(1000, "b"));
        // 로그인 없는 슬롯은 작업대를 건드리지 않는다 — 빈 자리를 주면 pane 이
        // 로그인 화면으로 뜬다.
        assert_eq!(swap_active_in(&active, "zzz", s.vault_of()), SwapOutcome::VaultEmpty);
        assert_eq!(read_credentials(Some(&active)).unwrap(), creds(1000, "b"));
    }

    /// **계정이 죽지 않는 이유**: 도는 세션이 작업대에서 토큰을 갱신하면 그 값을
    /// 금고로 되받는다. 안 되받으면 금고에 이미 쓴 refresh token 이 남아, 다음에 그
    /// 계정을 꺼낼 때 로그아웃된 채로 꺼내진다(1회용이라 되돌릴 수 없다).
    #[test]
    fn live_refresh_is_carried_back_into_the_vault() {
        let s = Slots::new("readback");
        let active = s.dir("active");
        assert!(write_credentials(Some(&s.dir("vault-a")), &creds(1000, "a")));
        assert_eq!(swap_active_in(&active, "a", s.vault_of()), SwapOutcome::Swapped);

        // 도는 claude 가 갱신했다 — 작업대에만 새 토큰이 있다.
        assert!(write_credentials(Some(&active), &creds(9000, "a2")));
        assert!(read_back_in(&active, s.vault_of()), "되받아야 한다");
        assert_eq!(read_credentials(Some(&s.dir("vault-a"))).unwrap(), creds(9000, "a2"));
        // 두 번째 호출은 할 일이 없다.
        assert!(!read_back_in(&active, s.vault_of()));

        // 그 뒤 다른 계정으로 갔다가 돌아와도 **갱신된** 토큰이 나온다.
        assert!(write_credentials(Some(&s.dir("vault-b")), &creds(1000, "b")));
        assert_eq!(swap_active_in(&active, "b", s.vault_of()), SwapOutcome::Swapped);
        assert_eq!(swap_active_in(&active, "a", s.vault_of()), SwapOutcome::Swapped);
        assert_eq!(read_credentials(Some(&active)).unwrap(), creds(9000, "a2"));
    }

    /// 낡은 값으로 금고를 덮지 않는다 — 옛 방식으로 묶인 pane 이 금고를 먼저 갱신한
    /// 경우다. 덮으면 그 계정이 통째로 로그아웃된다.
    #[test]
    fn read_back_never_overwrites_a_newer_vault() {
        let s = Slots::new("newer");
        let active = s.dir("active");
        assert!(write_credentials(Some(&s.dir("vault-a")), &creds(1000, "a")));
        assert_eq!(swap_active_in(&active, "a", s.vault_of()), SwapOutcome::Swapped);
        // 금고 쪽이 더 새것이 됐다(옛 pane 이 갱신).
        assert!(write_credentials(Some(&s.dir("vault-a")), &creds(9000, "vault-new")));
        // 작업대에는 그보다 낡은 값이 들어온다.
        assert!(write_credentials(Some(&active), &creds(2000, "active-old")));
        assert!(!read_back_in(&active, s.vault_of()), "낡은 값으로 덮으면 안 된다");
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
