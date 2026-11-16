//! 맥 로그인 키체인의 로그인 항목(일반 암호, 서비스 `web:<host>`, 계정 = 아이디). 브라우저와 같은 꼴이다
//! (kasabrowser-go internal/credentials/keychain_darwin.m).
//!
//! 새 항목은 접근 목록을 [kasaterm, 매일 브라우저] 로 두고 소유자(ChangeACL)도 둘 다로 한다 — 둘 다 창 없이 읽고
//! 바꾸고 지운다. 브라우저는 그 자리의 앱이 팀 서명·번들 id 요구조건을 채울 때만 넣는다 — 같은 사용자 권한의 다른
//! 프로그램이 그 자리에 앱을 두고 모든 항목의 믿는 앱이 되지 못하게. 있는 항목은 값만 제자리에서 바꾼다(`SecItemUpdate` 는 접근 목록을 그대로 둔다, 2026-11-16 실측).
//! 브라우저가 만든 항목은 소유자가 브라우저뿐이라 지우기가 -25244 로 거절된다 — 엔진이 「보류」로 센다.
//! 목록은 값 없이 속성만 읽어 허용 창이 뜨지 않는다.

#![allow(non_upper_case_globals)]

use std::ffi::{c_char, c_void, CStr, CString};

use core_foundation_sys::array::{kCFTypeArrayCallBacks, CFArrayCreate, CFArrayGetCount, CFArrayGetValueAtIndex, CFArrayRef};
use core_foundation_sys::base::{kCFAllocatorDefault, CFGetTypeID, CFIndex, CFRelease, CFTypeRef, OSStatus};
use core_foundation_sys::data::{CFDataCreate, CFDataGetBytePtr, CFDataGetLength, CFDataGetTypeID, CFDataRef};
use core_foundation_sys::date::{CFDateGetAbsoluteTime, CFDateGetTypeID, CFDateRef};
use core_foundation_sys::dictionary::{
    kCFTypeDictionaryKeyCallBacks, kCFTypeDictionaryValueCallBacks, CFDictionaryCreateMutable, CFDictionaryGetTypeID,
    CFDictionaryGetValue, CFDictionaryRef, CFDictionarySetValue, CFMutableDictionaryRef,
};
use core_foundation_sys::number::kCFBooleanTrue;
use core_foundation_sys::url::{CFURLCreateFromFileSystemRepresentation, CFURLRef};
use core_foundation_sys::string::{
    kCFStringEncodingUTF8, CFStringCreateWithBytes, CFStringGetCString, CFStringGetLength, CFStringGetMaximumSizeForEncoding,
    CFStringGetTypeID, CFStringRef,
};
use security_framework_sys::item::{
    kSecAttrAccount, kSecAttrLabel, kSecAttrService, kSecClass, kSecClassGenericPassword, kSecMatchLimit, kSecMatchLimitAll,
    kSecReturnAttributes, kSecReturnData, kSecValueData,
};
use security_framework_sys::keychain_item::{SecItemAdd, SecItemCopyMatching, SecItemDelete, SecItemUpdate};

use zeroize::Zeroizing;

use super::store::{parse_value, storable, value_json, Entry, LoginStore, OWNER_EDIT};

type SecAccessRef = *mut c_void;
type SecTrustedApplicationRef = *mut c_void;
type SecACLRef = *mut c_void;
type SecStaticCodeRef = *mut c_void;
type SecRequirementRef = *mut c_void;

// SecAccess·SecTrustedApplication 은 폐기 예정이지만 로그인 키체인 항목에 이름 붙인 앱 목록을 주는 길은 이것뿐이다.
#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    static kSecAttrAccess: CFStringRef;
    static kSecAttrModificationDate: CFStringRef;
    static kSecACLAuthorizationChangeACL: CFStringRef;
    fn SecTrustedApplicationCreateFromPath(path: *const c_char, app: *mut SecTrustedApplicationRef) -> OSStatus;
    fn SecAccessCreate(descriptor: CFStringRef, trusted: CFArrayRef, access: *mut SecAccessRef) -> OSStatus;
    fn SecAccessCopyMatchingACLList(access: SecAccessRef, tag: CFTypeRef) -> CFArrayRef;
    fn SecACLCopyContents(acl: SecACLRef, apps: *mut CFArrayRef, description: *mut CFStringRef, prompt: *mut u16) -> OSStatus;
    fn SecACLSetContents(acl: SecACLRef, apps: CFArrayRef, description: CFStringRef, prompt: u16) -> OSStatus;
    fn SecStaticCodeCreateWithPath(path: CFURLRef, flags: u32, code: *mut SecStaticCodeRef) -> OSStatus;
    fn SecRequirementCreateWithString(text: CFStringRef, flags: u32, requirement: *mut SecRequirementRef) -> OSStatus;
    fn SecStaticCodeCheckValidity(code: SecStaticCodeRef, flags: u32, requirement: SecRequirementRef) -> OSStatus;
    fn SecTrustedApplicationCopyRequirement(app: SecTrustedApplicationRef, requirement: *mut SecRequirementRef) -> OSStatus;
    fn SecCodeCopyDesignatedRequirement(code: SecStaticCodeRef, flags: u32, requirement: *mut SecRequirementRef) -> OSStatus;
    fn SecRequirementCopyData(requirement: SecRequirementRef, flags: u32, data: *mut CFDataRef) -> OSStatus;
}

/// 매일 브라우저의 서명 요구조건 — 이 팀이 이 번들 id 로 서명한 앱만.
const BROWSER_REQUIREMENT: &str =
    "identifier \"dev.kasa.browser-osr\" and anchor apple generic and certificate leaf[subject.OU] = \"L366799VND\"";
/// 실행 파일까지는 확인하되 번들의 모든 자원 해시는 건너뛴다 — 큰 브라우저 번들을 새 항목마다 다 읽지 않게.
const CS_DO_NOT_VALIDATE_RESOURCES: u32 = 1 << 2;

const NOT_FOUND: OSStatus = -25300;
const INVALID_OWNER_EDIT: OSStatus = -25244;
const ACL_NOT_SIMPLE: OSStatus = -25240;
/// 2001-01-01 (CFAbsoluteTime 의 0) 의 유닉스 초.
const CF_EPOCH: f64 = 978_307_200.0;
/// 매일 브라우저. 없으면 /Applications 를 본다.
const BROWSER_APP: &str = "Applications/Kasa Browser OSR.app";

/// 만든 것을 놓을 때 CFRelease 하는 칸.
struct Cf(CFTypeRef);

impl Drop for Cf {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: 만들기(Create·Copy) 규칙으로 받은 것만 담는다.
            unsafe { CFRelease(self.0) };
        }
    }
}

fn cfstr(text: &str) -> Cf {
    // SAFETY: 바이트와 길이는 같은 &str 에서 온다.
    Cf(unsafe { CFStringCreateWithBytes(kCFAllocatorDefault, text.as_ptr(), text.len() as CFIndex, kCFStringEncodingUTF8, 0) }.cast())
}

/// # Safety
/// `value` 는 null 이거나 살아 있는 CF 객체여야 한다(빌려 온 것도 된다).
unsafe fn string_of(value: *const c_void) -> Option<String> {
    if value.is_null() || unsafe { CFGetTypeID(value) } != unsafe { CFStringGetTypeID() } {
        return None;
    }
    let s = value as CFStringRef;
    let max = unsafe { CFStringGetMaximumSizeForEncoding(CFStringGetLength(s), kCFStringEncodingUTF8) } + 1;
    let mut buf = vec![0u8; max as usize];
    if unsafe { CFStringGetCString(s, buf.as_mut_ptr() as *mut c_char, max, kCFStringEncodingUTF8) } == 0 {
        return None;
    }
    CStr::from_bytes_until_nul(&buf).ok()?.to_str().ok().map(str::to_string)
}

fn mutable_dict() -> Cf {
    // SAFETY: 기본 콜백으로 빈 사전을 만든다.
    Cf(unsafe { CFDictionaryCreateMutable(kCFAllocatorDefault, 0, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks) }.cast())
}

fn set(dict: &Cf, key: CFStringRef, value: *const c_void) {
    // SAFETY: dict 는 mutable_dict 가 만든 것이고, 사전이 키·값을 retain 한다.
    unsafe { CFDictionarySetValue(dict.0 as CFMutableDictionaryRef, key.cast(), value) };
}

fn query(service: &str, account: Option<&str>) -> (Cf, Vec<Cf>) {
    let q = mutable_dict();
    let mut keep = Vec::new();
    // SAFETY: Security 프레임워크의 상수는 프로세스 동안 산다.
    unsafe {
        set(&q, kSecClass, kSecClassGenericPassword.cast());
        let s = cfstr(service);
        set(&q, kSecAttrService, s.0);
        keep.push(s);
        if let Some(account) = account {
            let a = cfstr(account);
            set(&q, kSecAttrAccount, a.0);
            keep.push(a);
        }
    }
    (q, keep)
}

fn data(bytes: &[u8]) -> Cf {
    // SAFETY: 바이트와 길이는 같은 슬라이스에서 온다. CFData 가 복사해 간다.
    Cf(unsafe { CFDataCreate(kCFAllocatorDefault, bytes.as_ptr(), bytes.len() as CFIndex) }.cast())
}

fn trusted_app(path: Option<&str>) -> Option<Cf> {
    let c = match path {
        Some(p) => Some(CString::new(p).ok()?),
        None => None,
    };
    let mut app: SecTrustedApplicationRef = std::ptr::null_mut();
    // SAFETY: path 가 없으면 이 실행 파일을 가리킨다. 성공하면 app 은 만든 것이다.
    let status = unsafe { SecTrustedApplicationCreateFromPath(c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()), &mut app) };
    (status == 0 && !app.is_null()).then(|| Cf(app as CFTypeRef))
}

/// 요구조건의 컴파일된 바이트 — 글로 견주면 「… or identifier "다른 것"」 같은 꼬리에 속는다.
fn requirement_bytes(requirement: SecRequirementRef) -> Option<Vec<u8>> {
    // SAFETY: requirement 는 살아 있는 요구조건이다. 받은 CFData 는 Copy 규칙이라 Cf 가 놓는다.
    unsafe {
        let mut data: CFDataRef = std::ptr::null();
        if SecRequirementCopyData(requirement, 0, &mut data) != 0 || data.is_null() {
            return None;
        }
        let data = Cf(data.cast());
        let len = CFDataGetLength(data.0 as CFDataRef) as usize;
        Some(std::slice::from_raw_parts(CFDataGetBytePtr(data.0 as CFDataRef), len).to_vec())
    }
}

/// 그 자리의 앱이 브라우저 요구조건을 채우면, 확인을 통과한 그 코드의 지정 요구조건 바이트를 준다.
fn browser_designated(path: &std::path::Path) -> Option<Vec<u8>> {
    let bytes = path.to_string_lossy().into_owned().into_bytes();
    // SAFETY: 경로 바이트와 길이는 같은 Vec 에서 온다. 만든 것은 모두 Cf 가 놓는다.
    unsafe {
        let url = Cf(CFURLCreateFromFileSystemRepresentation(kCFAllocatorDefault, bytes.as_ptr(), bytes.len() as CFIndex, 1).cast());
        if url.0.is_null() {
            return None;
        }
        let mut code: SecStaticCodeRef = std::ptr::null_mut();
        if SecStaticCodeCreateWithPath(url.0 as CFURLRef, 0, &mut code) != 0 || code.is_null() {
            return None;
        }
        let code = Cf(code as CFTypeRef);
        let requirement = compile(BROWSER_REQUIREMENT)?;
        if SecStaticCodeCheckValidity(code.0 as SecStaticCodeRef, CS_DO_NOT_VALIDATE_RESOURCES, requirement.0 as SecRequirementRef) != 0 {
            return None;
        }
        let mut designated: SecRequirementRef = std::ptr::null_mut();
        if SecCodeCopyDesignatedRequirement(code.0 as SecStaticCodeRef, 0, &mut designated) != 0 || designated.is_null() {
            return None;
        }
        let designated = Cf(designated as CFTypeRef);
        requirement_bytes(designated.0 as SecRequirementRef)
    }
}

fn compile(text: &str) -> Option<Cf> {
    let text = cfstr(text);
    let mut requirement: SecRequirementRef = std::ptr::null_mut();
    // SAFETY: 글은 위에서 만든 CFString 이다. 받은 요구조건은 Create 규칙이라 Cf 가 놓는다.
    let status = unsafe { SecRequirementCreateWithString(text.0 as CFStringRef, 0, &mut requirement) };
    (status == 0 && !requirement.is_null()).then(|| Cf(requirement as CFTypeRef))
}

/// 믿는 앱 기록이 실제로 담은 요구조건의 바이트. 키체인은 이 요구조건으로 앱을 알아본다.
fn recorded_requirement(app: &Cf) -> Option<Vec<u8>> {
    // SAFETY: app 은 SecTrustedApplicationCreateFromPath 가 만든 것이다. 받은 요구조건은 Copy 규칙이라 Cf 가 놓는다.
    unsafe {
        let mut requirement: SecRequirementRef = std::ptr::null_mut();
        if SecTrustedApplicationCopyRequirement(app.0 as SecTrustedApplicationRef, &mut requirement) != 0 || requirement.is_null() {
            return None;
        }
        let requirement = Cf(requirement as CFTypeRef);
        requirement_bytes(requirement.0 as SecRequirementRef)
    }
}

/// 서명이 맞는 매일 브라우저. 확인을 통과한 코드의 지정 요구조건과 믿는 앱 기록의 요구조건이 바이트까지 같을 때만 준다 —
/// 경로를 확인한 뒤 다시 열어 기록을 만드므로, 그 사이 번들을 바꿔치기했으면 여기서 걸린다. 없거나 안 맞으면 kasaterm 만
/// 믿는 항목이 되고, 브라우저가 처음 읽을 때 허용 창이 뜬다.
fn browser_app() -> Option<Cf> {
    let home = kasa_socket::home_dir().map(|h| h.join(BROWSER_APP));
    let system = std::path::Path::new("/").join(BROWSER_APP);
    let path = [home, Some(system)].into_iter().flatten().find(|p| p.exists())?;
    let designated = browser_designated(&path)?;
    let app = trusted_app(Some(&path.to_string_lossy()))?;
    (recorded_requirement(&app)? == designated).then_some(app)
}

fn cf_array(items: &[&Cf]) -> Cf {
    let values: Vec<*const c_void> = items.iter().map(|c| c.0).collect();
    // SAFETY: 기본 콜백이 항목을 retain 한다.
    Cf(unsafe { CFArrayCreate(kCFAllocatorDefault, values.as_ptr(), values.len() as CFIndex, &kCFTypeArrayCallBacks) }.cast())
}

/// 접근 목록에서 `tag` 권한을 가진 칸을 모두 `apps` 로 좁힌다.
fn only_apps(access: SecAccessRef, tag: CFStringRef, apps: &Cf) -> OSStatus {
    // SAFETY: access 는 SecAccessCreate 가 만든 것이다. 받은 목록과 내용은 Copy 규칙이라 놓는다.
    unsafe {
        let acls = Cf(SecAccessCopyMatchingACLList(access, tag.cast()).cast());
        if acls.0.is_null() || CFArrayGetCount(acls.0 as CFArrayRef) == 0 {
            return ACL_NOT_SIMPLE;
        }
        for i in 0..CFArrayGetCount(acls.0 as CFArrayRef) {
            let acl = CFArrayGetValueAtIndex(acls.0 as CFArrayRef, i) as SecACLRef;
            let (mut old, mut description, mut prompt): (CFArrayRef, CFStringRef, u16) = (std::ptr::null(), std::ptr::null(), 0);
            let mut status = SecACLCopyContents(acl, &mut old, &mut description, &mut prompt);
            let (_old, _description) = (Cf(old.cast()), Cf(description.cast()));
            if status == 0 {
                status = SecACLSetContents(acl, apps.0 as CFArrayRef, description, prompt);
            }
            if status != 0 {
                return status;
            }
        }
        0
    }
}

pub struct MacStore {
    prefix: String,
}

impl MacStore {
    pub fn new(prefix: &str) -> Self {
        Self { prefix: prefix.into() }
    }

    fn service(&self, host: &str) -> String {
        format!("{}{host}", self.prefix)
    }

    /// 속성만 — 값을 안 읽으니 허용 창이 없다. `service` 가 없으면 모든 일반 암호.
    fn attributes(&self, service: Option<&str>) -> anyhow::Result<Vec<(String, String, Option<u64>)>> {
        let (q, _keep) = match service {
            Some(service) => query(service, None),
            None => {
                let q = mutable_dict();
                // SAFETY: 상수는 프로세스 동안 산다.
                unsafe { set(&q, kSecClass, kSecClassGenericPassword.cast()) };
                (q, Vec::new())
            }
        };
        let mut out = Vec::new();
        // SAFETY: 질의는 위에서 만든 사전이고, 결과는 Copy 규칙이라 Cf 가 놓는다. 사전 안의 값은 빌려 읽는다.
        unsafe {
            set(&q, kSecMatchLimit, kSecMatchLimitAll.cast());
            set(&q, kSecReturnAttributes, kCFBooleanTrue.cast());
            let mut result: CFTypeRef = std::ptr::null();
            match SecItemCopyMatching(q.0 as CFDictionaryRef, &mut result) {
                0 => {}
                NOT_FOUND => return Ok(out),
                status => anyhow::bail!("keychain: {status}"),
            }
            let result = Cf(result);
            let list = result.0 as CFArrayRef;
            for i in 0..CFArrayGetCount(list) {
                let item = CFArrayGetValueAtIndex(list, i);
                if item.is_null() || CFGetTypeID(item) != CFDictionaryGetTypeID() {
                    continue;
                }
                let dict = item as CFDictionaryRef;
                let Some(svce) = string_of(CFDictionaryGetValue(dict, kSecAttrService.cast())) else { continue };
                let acct = string_of(CFDictionaryGetValue(dict, kSecAttrAccount.cast())).unwrap_or_default();
                let date = CFDictionaryGetValue(dict, kSecAttrModificationDate.cast());
                let modified = (!date.is_null() && CFGetTypeID(date) == CFDateGetTypeID())
                    .then(|| ((CFDateGetAbsoluteTime(date as CFDateRef) + CF_EPOCH) * 1000.0) as u64);
                out.push((svce, acct, modified));
            }
        }
        Ok(out)
    }

    /// 그 사이트에서 아이디가 대소문자만 다른 항목의 실제 철자.
    fn spelling(&self, host: &str, username: &str) -> anyhow::Result<Option<String>> {
        let service = self.service(host);
        let accounts: Vec<String> = self.attributes(Some(&service))?.into_iter().map(|(_, acct, _)| acct).collect();
        if accounts.iter().any(|a| a == username) {
            return Ok(Some(username.to_string()));
        }
        let wanted = username.to_lowercase();
        Ok(accounts.into_iter().find(|a| a.to_lowercase() == wanted))
    }

    fn add(&self, service: &str, account: &str, value: &[u8]) -> anyhow::Result<()> {
        let me = trusted_app(None).ok_or_else(|| anyhow::anyhow!("keychain: trusted_app"))?;
        let browser = browser_app();
        let apps = match &browser {
            Some(b) => cf_array(&[&me, b]),
            None => cf_array(&[&me]),
        };
        let label = cfstr(service);
        let mut access: SecAccessRef = std::ptr::null_mut();
        // SAFETY: 목록·이름은 위에서 만든 것이다. 성공하면 access 는 만든 것이다.
        let status = unsafe { SecAccessCreate(label.0 as CFStringRef, apps.0 as CFArrayRef, &mut access) };
        let access = Cf(access as CFTypeRef);
        anyhow::ensure!(status == 0, "keychain: {status}");
        // SecAccessCreate 는 소유자를 비워 둔다 — 그대로면 이 실행이 끝난 뒤 키체인 암호 없이는 아무도 못 지운다.
        let status = only_apps(access.0 as SecAccessRef, unsafe { kSecACLAuthorizationChangeACL }, &apps);
        anyhow::ensure!(status == 0, "keychain: {status}");
        let (item, _keep) = query(service, Some(account));
        let value = data(value);
        // SAFETY: 사전과 값은 위에서 만든 것이다.
        let status = unsafe {
            set(&item, kSecAttrLabel, label.0);
            set(&item, kSecAttrAccess, access.0);
            set(&item, kSecValueData, value.0);
            SecItemAdd(item.0 as CFDictionaryRef, std::ptr::null_mut())
        };
        anyhow::ensure!(status == 0, "keychain: {status}");
        Ok(())
    }
}

impl LoginStore for MacStore {
    fn list(&self) -> anyhow::Result<Vec<Entry>> {
        Ok(self
            .attributes(None)?
            .into_iter()
            .filter_map(|(svce, acct, modified)| {
                let host = svce.strip_prefix(&self.prefix)?;
                Some(Entry { host: host.to_string(), username: acct, modified_ms: modified })
            })
            .collect())
    }

    fn read(&self, host: &str, username: &str) -> anyhow::Result<Option<Zeroizing<String>>> {
        let Some(account) = self.spelling(host, username)? else { return Ok(None) };
        let (q, _keep) = query(&self.service(host), Some(&account));
        // SAFETY: 질의는 위에서 만든 사전이고, 결과 CFData 는 Copy 규칙이라 Cf 가 놓는다.
        unsafe {
            set(&q, kSecReturnData, kCFBooleanTrue.cast());
            let mut result: CFTypeRef = std::ptr::null();
            match SecItemCopyMatching(q.0 as CFDictionaryRef, &mut result) {
                0 => {}
                NOT_FOUND => return Ok(None),
                status => anyhow::bail!("keychain: {status}"),
            }
            let result = Cf(result);
            if result.0.is_null() || CFGetTypeID(result.0) != CFDataGetTypeID() {
                return Ok(None);
            }
            let bytes = std::slice::from_raw_parts(CFDataGetBytePtr(result.0 as CFDataRef), CFDataGetLength(result.0 as CFDataRef) as usize);
            let raw = Zeroizing::new(String::from_utf8_lossy(bytes).into_owned());
            Ok(Some(parse_value(&raw, &account).1))
        }
    }

    fn write(&self, host: &str, username: &str, password: &str) -> anyhow::Result<()> {
        anyhow::ensure!(storable(host, username), "not_storable");
        let service = self.service(host);
        let value = value_json(username, password);
        match self.spelling(host, username)? {
            Some(existing) => {
                let (q, _keep) = query(&service, Some(&existing));
                let change = mutable_dict();
                let new_value = data(value.as_bytes());
                let name = cfstr(username);
                // SAFETY: 사전·값은 위에서 만든 것이다. 접근 목록은 처음 만든 앱이 정한 대로 남는다.
                let status = unsafe {
                    set(&change, kSecValueData, new_value.0);
                    if existing != username {
                        set(&change, kSecAttrAccount, name.0);
                    }
                    SecItemUpdate(q.0 as CFDictionaryRef, change.0 as CFDictionaryRef)
                };
                if status == 0 { Ok(()) } else { Err(anyhow::anyhow!("keychain: {status}")) }
            }
            None => self.add(&service, username, value.as_bytes()),
        }
    }

    fn delete(&self, host: &str, username: &str) -> anyhow::Result<()> {
        let Some(account) = self.spelling(host, username)? else { return Ok(()) };
        let (q, _keep) = query(&self.service(host), Some(&account));
        // SAFETY: 질의는 위에서 만든 사전이다.
        match unsafe { SecItemDelete(q.0 as CFDictionaryRef) } {
            0 | NOT_FOUND => Ok(()),
            INVALID_OWNER_EDIT => anyhow::bail!(OWNER_EDIT),
            status => anyhow::bail!("keychain: {status}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::store::TEST_PREFIX;
    use super::*;

    /// 요구조건은 컴파일된 바이트로 견준다 — 같은 글이면 같고, 「or」 꼬리를 붙인 글은 다르다.
    #[test]
    fn requirements_compare_by_exact_bytes() {
        let bytes = |text: &str| requirement_bytes(compile(text).expect("compiles").0 as SecRequirementRef).expect("bytes");
        let real = bytes(BROWSER_REQUIREMENT);
        assert_eq!(real, bytes(BROWSER_REQUIREMENT));
        assert_ne!(real, bytes(&format!("{BROWSER_REQUIREMENT} or identifier \"dev.evil.app\"")), "「or」 꼬리는 다른 요구조건이다");
        assert_ne!(real, bytes(&BROWSER_REQUIREMENT.replace("L366799VND", "AAAAAAAAAA")));
    }

    /// 이 맥에 매일 브라우저가 깔려 있으면, 그 앱으로 만든 믿는 앱 기록의 요구조건이 확인한 코드의 지정 요구조건과 같아야 한다.
    #[test]
    #[ignore = "맥에 깔린 매일 브라우저를 읽는다 — 손으로 돌린다"]
    fn the_installed_browser_is_recorded_with_its_designated_requirement() {
        let home = kasa_socket::home_dir().map(|h| h.join(BROWSER_APP)).filter(|p| p.exists());
        let Some(path) = home else { return };
        let designated = browser_designated(&path).expect("signed as the browser");
        let app = trusted_app(Some(&path.to_string_lossy())).expect("trusted app");
        assert_eq!(recorded_requirement(&app).expect("recorded"), designated);
        let me = trusted_app(None).expect("this test binary");
        assert_ne!(recorded_requirement(&me), Some(designated), "다른 실행 파일의 기록은 브라우저가 아니다");
        assert!(browser_app().is_some());
    }

    /// 진짜 로그인 키체인을 시험 접두사로만 쓴다 — 진짜 `web:` 항목은 건드리지 않는다. 손으로 돌린다:
    /// `cargo test -p kasa-device --lib macos_store -- --ignored`
    /// 소유자 판정이 실행 파일 경로라, 시험 실행 파일이 만든 항목을 시험 실행 파일이 지우는 것까지만 잰다.
    #[test]
    #[ignore = "로그인 키체인에 시험 항목을 썼다 지운다 — 손으로 돌린다"]
    fn login_items_with_the_test_prefix() {
        let store = MacStore::new(TEST_PREFIX);
        let host = format!("probe-{}.example", std::process::id());
        // 중간 단언이 실패해도 시험 항목을 남기지 않는다 — 시험이 만든 항목이라 시험이 지울 수 있다.
        struct Cleanup(String);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let store = MacStore::new(TEST_PREFIX);
                for entry in store.list().unwrap_or_default().into_iter().filter(|e| e.host == self.0) {
                    let _ = store.delete(&entry.host, &entry.username);
                }
            }
        }
        let _cleanup = Cleanup(host.clone());
        let mine = |store: &MacStore| -> Vec<String> {
            let mut names: Vec<String> = store.list().unwrap().into_iter().filter(|e| e.host == host).map(|e| e.username).collect();
            names.sort();
            names
        };
        store.write(&host, "First@Example.com", "p1").unwrap();
        store.write(&host, "second", "p2").unwrap();
        assert_eq!(mine(&store), vec!["First@Example.com".to_string(), "second".to_string()]);
        assert!(store.list().unwrap().iter().filter(|e| e.host == host).all(|e| e.modified_ms.is_some()), "수정 시각을 읽는다");
        assert_eq!(store.read(&host, "first@example.com").unwrap().as_deref().map(String::as_str), Some("p1"), "아이디는 대소문자를 가리지 않는다");
        store.write(&host, "SECOND", "p3").unwrap();
        assert_eq!(mine(&store), vec!["First@Example.com".to_string(), "SECOND".to_string()], "제자리에서 철자만 바뀐다");
        assert_eq!(store.read(&host, "second").unwrap().as_deref().map(String::as_str), Some("p3"));
        store.delete(&host, "first@example.com").unwrap();
        store.delete(&host, "second").unwrap();
        assert!(mine(&store).is_empty(), "시험 항목이 남지 않는다");
    }
}
