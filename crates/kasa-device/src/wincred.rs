//! Windows 자격 증명 관리자의 일반 자격 증명 — 키체인 자리. 맥의 「서비스·계정」은 「대상·사용자」다
//! (`cmdkey /generic:<서비스> /user:<계정> /pass` 가 만드는 꼴). 폰 승인 비밀 읽기와 로그인 동기화가 함께 쓴다.

use windows::core::{HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{ERROR_NOT_FOUND, FILETIME};
use windows::Win32::Security::Credentials::{
    CredDeleteW, CredEnumerateW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_ENUMERATE_ALL_CREDENTIALS,
    CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC,
};

pub(crate) struct Cred {
    pub target: String,
    pub blob: Vec<u8>,
    pub user: String,
    pub comment: String,
}

/// 목록 한 줄 — 값은 없다.
pub(crate) struct CredInfo {
    pub target: String,
    pub user: String,
    /// 마지막으로 쓴 때(유닉스 ms).
    pub written_ms: u64,
}

fn filetime_ms(t: FILETIME) -> u64 {
    // FILETIME 은 1601-01-01 부터 100ns 단위다.
    let ticks = ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64;
    (ticks / 10_000).saturating_sub(11_644_473_600_000)
}

fn text(p: PWSTR) -> String {
    if p.is_null() { String::new() } else { unsafe { p.to_string() }.unwrap_or_default() }
}

/// OS 가 준 항목의 값 칸을 놓기 전에 지운다 — 값은 이 프로세스 힙에 남지 않아야 한다.
///
/// # Safety
/// `c` 는 CredReadW·CredEnumerateW 가 준, 아직 CredFree 하지 않은 항목이어야 한다.
unsafe fn wipe(c: &CREDENTIALW) {
    if !c.CredentialBlob.is_null() {
        // SAFETY: 크기는 같은 항목이 준 CredentialBlobSize 이고, 그 버퍼는 이 프로세스에 내준 것이다.
        unsafe { std::ptr::write_bytes(c.CredentialBlob, 0, c.CredentialBlobSize as usize) };
    }
}

pub(crate) fn read(target: &str) -> anyhow::Result<Option<Cred>> {
    let mut found: *mut CREDENTIALW = std::ptr::null_mut();
    match unsafe { CredReadW(&HSTRING::from(target), CRED_TYPE_GENERIC, None, &mut found) } {
        // SAFETY: 성공하면 found 는 유효한 항목이고, 복사한 뒤 값 칸을 지우고 돌려준다.
        Ok(()) => unsafe {
            let c = &*found;
            let blob = if c.CredentialBlob.is_null() {
                Vec::new()
            } else {
                std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize).to_vec()
            };
            let cred = Cred { target: text(c.TargetName), blob, user: text(c.UserName), comment: text(c.Comment) };
            wipe(c);
            CredFree(found as *const _);
            Ok(Some(cred))
        },
        Err(error) if error.code() == ERROR_NOT_FOUND.to_hresult() => Ok(None),
        Err(error) => anyhow::bail!("credential: {:#x}", error.code().0),
    }
}

/// 이 사용자의 일반 자격 증명 중 이름이 `keep` 에 드는 것의 속성. OS 는 값까지 함께 주지만 읽지 않고 지운 뒤 놓는다 —
/// 값은 바뀐 항목만 [`read`] 로 따로 읽는다.
pub(crate) fn enumerate(keep: impl Fn(&str) -> bool) -> anyhow::Result<Vec<CredInfo>> {
    let mut count = 0u32;
    let mut list: *mut *mut CREDENTIALW = std::ptr::null_mut();
    match unsafe { CredEnumerateW(PCWSTR::null(), Some(CRED_ENUMERATE_ALL_CREDENTIALS), &mut count, &mut list) } {
        // SAFETY: 성공하면 list 는 count 개의 유효한 포인터 배열이고, 다 복사한 뒤 한 번에 돌려준다.
        Ok(()) => unsafe {
            let items = std::slice::from_raw_parts(list, count as usize);
            let mut creds = Vec::new();
            for &c in items.iter().filter(|c| !c.is_null()) {
                let c = &*c;
                wipe(c);
                if c.Type != CRED_TYPE_GENERIC {
                    continue;
                }
                let name = text(c.TargetName);
                // 전부 훑기(ALL_CREDENTIALS)는 일반 자격 증명 이름 앞에 이 머리를 붙여 준다. 읽기·지우기는 머리 없는 이름을 받는다.
                let plain = name.strip_prefix("LegacyGeneric:target=").unwrap_or(&name).to_string();
                if keep(&plain) {
                    creds.push(CredInfo { target: plain, user: text(c.UserName), written_ms: filetime_ms(c.LastWritten) });
                }
            }
            CredFree(list as *const _);
            Ok(creds)
        },
        Err(error) if error.code() == ERROR_NOT_FOUND.to_hresult() => Ok(Vec::new()),
        Err(error) => anyhow::bail!("credential: {:#x}", error.code().0),
    }
}

pub(crate) fn write(target: &str, user: &str, blob: &[u8], comment: &str) -> anyhow::Result<()> {
    let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let (mut target, mut comment, mut user) = (wide(target), wide(comment), wide(user));
    let cred = CREDENTIALW {
        Type: CRED_TYPE_GENERIC,
        TargetName: PWSTR(target.as_mut_ptr()),
        Comment: PWSTR(comment.as_mut_ptr()),
        CredentialBlobSize: blob.len() as u32,
        CredentialBlob: blob.as_ptr() as *mut u8,
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        UserName: PWSTR(user.as_mut_ptr()),
        ..Default::default()
    };
    unsafe { CredWriteW(&cred, 0) }.map_err(|error| anyhow::anyhow!("credential: {:#x}", error.code().0))
}

pub(crate) fn delete(target: &str) {
    let _ = unsafe { CredDeleteW(&HSTRING::from(target), CRED_TYPE_GENERIC, None) };
}

/// 남이 넣은 값은 UTF-16 으로 적힌 것이 많다(cmdkey·Python keyring·Git 자격 증명 관리자). ASCII 의 UTF-16 은
/// 늘 0 바이트를 품고, UTF-8 문자열은 0 을 안 품으니 그것으로 가른다. 끝의 NUL 은 C 문자열 꼬리라 뗀다.
pub(crate) fn decode(mut blob: Vec<u8>) -> Vec<u8> {
    use zeroize::Zeroize as _;
    if !blob.contains(&0) && std::str::from_utf8(&blob).is_ok() {
        return blob;
    }
    if blob.len() % 2 == 0 {
        let mut units: Vec<u16> = blob.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
        if let Ok(mut text) = String::from_utf16(&units) {
            let out = text.trim_end_matches('\0').as_bytes().to_vec();
            text.zeroize();
            units.zeroize();
            blob.zeroize();
            return out;
        }
        units.zeroize();
    }
    blob
}
