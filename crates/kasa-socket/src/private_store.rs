//! Windows 소유자 전용 저장소 — Unix 의 0700 디렉터리·0600 파일·`O_NOFOLLOW`·uid 검사에 대응한다.
//! tell 영수증 장부(`tell::Ledger`)와 관문·기기 보관함(kasa-mcp `sealed::Vault`)이 쓴다.
//!
//! 접근은 이 프로세스 토큰의 사용자 SID 하나에만 연다 — 허용 ACE 하나, 상속 차단. SYSTEM·Administrators 는
//! 넣지 않는다. Unix 0700 이 root 를 적지 않는 것과 같고, 관리자는 어차피 소유권 가져오기·백업 권한으로
//! 넘을 수 있어 ACE 로 따로 열 까닭이 없다.
//!
//! - 새 객체는 처음부터 그 보안 설명자로 만든다(`CreateDirectoryW`·`CreateFileW` 의 SECURITY_ATTRIBUTES).
//!   만든 뒤 좁히면 그 사이 상속 ACL 로 열린 남의 핸들이 접근권을 쥔 채 남는다.
//! - 설명자에 소유자를 사용자 SID 로 적는다. 관리자 토큰의 기본 소유자(TokenOwner)는 Administrators 그룹일
//!   수 있어(learn.microsoft.com 「Owner of a New Object」·「TOKEN_OWNER」), 적지 않으면 관리자 실행에서만
//!   소유자 검사가 깨지거나, 검사를 느슨하게 하면 그룹 소유를 받아 주게 된다.
//! - `CreateFileW` 는 이미 있는 파일에선 설명자를 무시한다 — 그래서 기존 것은 따로 검사한다. 검사는 열린
//!   핸들로 한다(`GetSecurityInfo`). 경로로 다시 물으면 검사와 사용 사이에 바뀔 수 있다.
//! - 대상은 `FILE_FLAG_OPEN_REPARSE_POINT` 로 열어 심볼릭 링크·정션·마운트 지점 같은 재분석 지점을 따라가지
//!   않고 거부한다(Rust `is_symlink` 는 이름 대리 태그만 잡는다). 상위 경로 구성요소는 Unix 처럼 따지지 않는다.
//! - 어느 단계든 실패하면 Err — 권한 확인 없이 쓰는 길은 없다. NULL DACL(모두 허용)·DACL 없음(FAT)도 거부.

use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{LocalFree, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
use windows_sys::Win32::Security::{
    AclSizeInformation, AddAccessAllowedAceEx, EqualSid, GetAce, GetAclInformation, GetLengthSid,
    GetSecurityDescriptorControl, GetTokenInformation, InitializeAcl, InitializeSecurityDescriptor, IsValidSid,
    SetSecurityDescriptorControl, SetSecurityDescriptorDacl, SetSecurityDescriptorOwner, TokenUser,
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_REVISION, ACL_SIZE_INFORMATION, CONTAINER_INHERIT_ACE,
    DACL_SECURITY_INFORMATION, INHERITED_ACE, OBJECT_INHERIT_ACE, OWNER_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR,
    SE_DACL_PRESENT, SE_DACL_PROTECTED, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateDirectoryW, CreateFileW, GetFileInformationByHandle, GetFileType, BY_HANDLE_FILE_INFORMATION, CREATE_NEW,
    FILE_ALL_ACCESS, FILE_ATTRIBUTE_DIRECTORY, FILE_LIST_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TYPE_DISK, OPEN_EXISTING, READ_CONTROL,
};
use windows_sys::Win32::System::SystemServices::{ACCESS_ALLOWED_ACE_TYPE, SECURITY_DESCRIPTOR_REVISION};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// 파일 핸들은 std 의 기본 공유 방식과 같다 — 잠금은 `File::try_lock`(LockFileEx)이 맡고, rename 은 원본을 닫은 뒤 한다.
const SHARE_ALL: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
/// 디렉터리 핸들은 지우기 공유를 빼서 쥐고 있는 동안 그 디렉터리의 이름 바꾸기·지우기를 막는다.
const SHARE_NO_DELETE: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE;
/// 디렉터리 핸들의 접근. 공유 검사는 데이터 접근(읽기·쓰기·지우기·실행)을 연 핸들만 센다 — 소유자·DACL·속성만
/// 읽는 핸들은 공유 모드가 무시돼 이름 바꾸기를 못 막는다(러너 실측). 그래서 목록 읽기를 함께 연다.
const DIR_ACCESS: u32 = READ_CONTROL | FILE_READ_ATTRIBUTES | FILE_LIST_DIRECTORY;

/// 검사한 디렉터리 핸들. 쥐고 있는 동안 그 디렉터리를 치우고 같은 이름에 정션을 세우는 바꿔치기가 막힌다 —
/// 안의 파일은 경로로 다시 여는데, 파일 쪽 `OPEN_REPARSE_POINT` 는 마지막 구성요소만 보기 때문이다.
/// 안에서 파일을 만들고·바꾸고·지우는 건 막지 않는다.
pub struct DirGuard {
    _handle: OwnedHandle,
}

/// 소유자 전용 디렉터리를 만든다. 이미 있으면 `AlreadyExists`.
pub fn create_dir(dir: &Path) -> io::Result<DirGuard> {
    let user = User::current()?;
    let acl = Acl::owner_only(&user, true)?;
    let path = wide(dir)?;
    if with_attributes(&user, &acl, |sa| unsafe { CreateDirectoryW(path.as_ptr(), sa) })? == 0 {
        return Err(io::Error::last_os_error());
    }
    // 보안을 모르는 파일 시스템(FAT)은 설명자를 조용히 버린다 — 만든 뒤 한 번 더 본다.
    verify_dir(dir)
}

/// 있는 디렉터리가 소유자 전용인지 본다. 넓으면 고치지 않고 거부한다(Vault — Unix 의 `mode & 077` 거부).
pub fn verify_dir(dir: &Path) -> io::Result<DirGuard> {
    let user = User::current()?;
    let handle = open(dir, DIR_ACCESS, true)?;
    require(shape_of(&handle, &user)?, true)?;
    Ok(DirGuard { _handle: handle })
}

/// 없으면 소유자 전용으로 만들고, 있으면 소유자 전용인지 본다. tell 이 쓴다.
///
/// Unix 는 이미 있는 폴더를 `chmod 0700` 으로 바로잡지만 여기선 바로잡지 않고 거부한다. 디렉터리 DACL 을 쓰면
/// OS 가 상속 ACE 를 기존 자식에게까지 퍼뜨려(learn.microsoft.com 「SetSecurityInfo」 Remarks) 남의 파일이나
/// 따로 DACL 을 둔 파일까지 바뀐다 — 부모만 바꾸는 chmod 와 달라진다.
pub fn ensure_dir(dir: &Path) -> io::Result<DirGuard> {
    match create_dir(dir) {
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => verify_dir(dir),
        other => other,
    }
}

/// 소유자 전용 새 파일(`CREATE_NEW` — 있으면 `AlreadyExists`, 그 이름이 링크여도 따라가지 않는다).
pub fn create_new(path: &Path) -> io::Result<File> {
    let user = User::current()?;
    let acl = Acl::owner_only(&user, false)?;
    let name = wide(path)?;
    let raw = with_attributes(&user, &acl, |sa| unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            SHARE_ALL,
            sa,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        )
    })?;
    let handle = owned(raw)?;
    require(shape_of(&handle, &user)?, false)?;
    Ok(File::from(handle))
}

/// 있는 파일을 연다. 재분석 지점·다른 소유자·소유자 전용이 아닌 DACL 이면 거부, 없으면 `NotFound`.
pub fn open_existing(path: &Path, write: bool) -> io::Result<File> {
    let user = User::current()?;
    let handle = open(path, GENERIC_READ | if write { GENERIC_WRITE } else { 0 }, false)?;
    require(shape_of(&handle, &user)?, false)?;
    Ok(File::from(handle))
}

/// 잠금 파일처럼 있으면 검사해 열고 없으면 소유자 전용으로 만든다.
pub fn open_or_create(path: &Path) -> io::Result<File> {
    match create_new(path) {
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => open_existing(path, true),
        other => other,
    }
}

/// 그 경로(따라가지 않고 그 자체)의 보안 모양. 진단·시험용으로 읽기만 한다.
pub fn describe(path: &Path) -> io::Result<Shape> {
    let user = User::current()?;
    let name = wide(path)?;
    let handle = owned(unsafe {
        CreateFileW(
            name.as_ptr(),
            READ_CONTROL | FILE_READ_ATTRIBUTES,
            SHARE_ALL,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
            null_mut(),
        )
    })?;
    shape_of(&handle, &user)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shape {
    pub reparse: bool,
    pub directory: bool,
    pub owner_is_user: bool,
    /// DACL 이 있다. 없으면(NULL DACL) 모두에게 열린 것이다.
    pub dacl_present: bool,
    pub dacl_protected: bool,
    /// 상속되지 않은, 현재 사용자 허용 ACE 수.
    pub user_aces: u32,
    /// 그 밖의 ACE — 다른 주체, 거부·감사 ACE, 상속된 ACE.
    pub other_aces: u32,
}

impl Shape {
    /// 이 모듈이 만드는 모양 그대로인가 — Unix owner-only 와 맞추려고 ACE 는 사용자 하나만 받는다.
    pub fn is_private(&self, directory: bool) -> bool {
        !self.reparse
            && self.directory == directory
            && self.owner_is_user
            && self.dacl_present
            && self.dacl_protected
            && self.user_aces == 1
            && self.other_aces == 0
    }
}

fn require(shape: Shape, directory: bool) -> io::Result<()> {
    if shape.is_private(directory) {
        Ok(())
    } else {
        Err(denied(&format!("not an owner-only {}: {shape:?}", if directory { "directory" } else { "file" })))
    }
}

fn denied(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, why.to_string())
}

/// Win32 경로 문자열. 안에 NUL 이 있으면 Win32 가 거기서 끊어 다른 대상을 열게 되니 std::fs 처럼 거부한다.
fn wide(path: &Path) -> io::Result<Vec<u16>> {
    let mut name: Vec<u16> = path.as_os_str().encode_wide().collect();
    if name.contains(&0) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL character"));
    }
    name.push(0);
    Ok(name)
}

fn raw(handle: &OwnedHandle) -> HANDLE {
    handle.as_raw_handle() as HANDLE
}

fn owned(raw: HANDLE) -> io::Result<OwnedHandle> {
    if raw == INVALID_HANDLE_VALUE || raw.is_null() {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedHandle::from_raw_handle(raw as _) })
}

/// 재분석 지점을 따라가지 않고 그 자체를 연다. 디렉터리 핸들은 BACKUP_SEMANTICS 가 있어야 얻는다.
fn open(path: &Path, access: u32, directory: bool) -> io::Result<OwnedHandle> {
    let name = wide(path)?;
    let (flags, share) = if directory {
        (FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS, SHARE_NO_DELETE)
    } else {
        (FILE_FLAG_OPEN_REPARSE_POINT, SHARE_ALL)
    };
    owned(unsafe { CreateFileW(name.as_ptr(), access, share, null(), OPEN_EXISTING, flags, null_mut()) })
}

fn shape_of(handle: &OwnedHandle, user: &User) -> io::Result<Shape> {
    unsafe {
        // 일반 파일 저장소다 — 장치·파이프·콘솔 핸들(`\\.\NUL` 같은)은 모양을 따지기 전에 거부한다.
        if GetFileType(raw(handle)) != FILE_TYPE_DISK {
            return Err(denied("not a file or directory on disk"));
        }
        let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        if GetFileInformationByHandle(raw(handle), &mut info) == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut owner: PSID = null_mut();
        let mut dacl: *mut ACL = null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
        let code = GetSecurityInfo(
            raw(handle),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut descriptor,
        );
        if code != 0 {
            return Err(io::Error::from_raw_os_error(code as i32));
        }
        let _descriptor = LocalMemory(descriptor);
        let mut control = 0u16;
        let mut revision = 0u32;
        if GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut shape = Shape {
            reparse: info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0,
            directory: info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0,
            owner_is_user: !owner.is_null() && EqualSid(owner, user.sid()) != 0,
            dacl_present: control & SE_DACL_PRESENT != 0 && !dacl.is_null(),
            dacl_protected: control & SE_DACL_PROTECTED != 0,
            user_aces: 0,
            other_aces: 0,
        };
        if shape.dacl_present {
            let mut size: ACL_SIZE_INFORMATION = std::mem::zeroed();
            if GetAclInformation(
                dacl,
                (&mut size as *mut ACL_SIZE_INFORMATION).cast(),
                std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            for index in 0..size.AceCount {
                let mut ace: *mut c_void = null_mut();
                if GetAce(dacl, index, &mut ace) == 0 {
                    return Err(io::Error::last_os_error());
                }
                let header = &*(ace as *const ACE_HEADER);
                let mine = header.AceType as u32 == ACCESS_ALLOWED_ACE_TYPE
                    && header.AceFlags as u32 & INHERITED_ACE == 0
                    && EqualSid(std::ptr::addr_of!((*(ace as *const ACCESS_ALLOWED_ACE)).SidStart) as PSID, user.sid()) != 0;
                if mine {
                    shape.user_aces += 1;
                } else {
                    shape.other_aces += 1;
                }
            }
        }
        Ok(shape)
    }
}

/// `GetSecurityInfo` 가 LocalAlloc 으로 준 설명자.
struct LocalMemory(PSECURITY_DESCRIPTOR);

impl Drop for LocalMemory {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { LocalFree(self.0) };
        }
    }
}

/// 이 프로세스 토큰의 사용자 SID. TOKEN_USER 버퍼째 들고 있어야 그 안의 SID 포인터가 산다.
struct User {
    buffer: Vec<u64>,
}

impl User {
    fn current() -> io::Result<Self> {
        unsafe {
            let mut token: HANDLE = null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return Err(io::Error::last_os_error());
            }
            let token = owned(token)?;
            let mut length = 0u32;
            GetTokenInformation(raw(&token), TokenUser, null_mut(), 0, &mut length);
            if length == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut buffer = vec![0u64; (length as usize).div_ceil(8)];
            if GetTokenInformation(raw(&token), TokenUser, buffer.as_mut_ptr().cast(), length, &mut length) == 0 {
                return Err(io::Error::last_os_error());
            }
            let user = Self { buffer };
            if IsValidSid(user.sid()) == 0 {
                return Err(denied("token user SID is invalid"));
            }
            Ok(user)
        }
    }

    fn sid(&self) -> PSID {
        unsafe { (*(self.buffer.as_ptr() as *const TOKEN_USER)).User.Sid }
    }
}

/// 사용자 SID 허용 ACE 하나뿐인 DACL. 디렉터리 것은 그 안에 새로 생기는 것에도 같은 ACE 가 이어진다.
struct Acl {
    buffer: Vec<u64>,
}

impl Acl {
    fn owner_only(user: &User, inherit: bool) -> io::Result<Self> {
        unsafe {
            let size = std::mem::size_of::<ACL>() + std::mem::size_of::<ACCESS_ALLOWED_ACE>()
                - std::mem::size_of::<u32>()
                + GetLengthSid(user.sid()) as usize;
            let size = (size + 3) & !3;
            let mut buffer = vec![0u64; size.div_ceil(8)];
            let acl = buffer.as_mut_ptr() as *mut ACL;
            if InitializeAcl(acl, size as u32, ACL_REVISION) == 0 {
                return Err(io::Error::last_os_error());
            }
            let flags = if inherit { OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE } else { 0 };
            if AddAccessAllowedAceEx(acl, ACL_REVISION, flags, FILE_ALL_ACCESS, user.sid()) == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self { buffer })
        }
    }

    fn as_ptr(&self) -> *const ACL {
        self.buffer.as_ptr().cast()
    }
}

/// 소유자 = 사용자, DACL = `acl`, 상속 차단인 설명자로 `create` 를 부른다.
fn with_attributes<T>(user: &User, acl: &Acl, create: impl FnOnce(*const SECURITY_ATTRIBUTES) -> T) -> io::Result<T> {
    unsafe {
        let mut descriptor: SECURITY_DESCRIPTOR = std::mem::zeroed();
        let pointer = (&mut descriptor as *mut SECURITY_DESCRIPTOR).cast::<c_void>();
        if InitializeSecurityDescriptor(pointer, SECURITY_DESCRIPTOR_REVISION) == 0
            || SetSecurityDescriptorOwner(pointer, user.sid(), 0) == 0
            || SetSecurityDescriptorDacl(pointer, 1, acl.as_ptr(), 0) == 0
            || SetSecurityDescriptorControl(pointer, SE_DACL_PROTECTED, SE_DACL_PROTECTED) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: pointer,
            bInheritHandle: 0,
        };
        Ok(create(&attributes))
    }
}

/// 시험 전용 — 일부러 넓히거나 바꿔 거부를 확인한다. 시험이 만든 임시 폴더에만 쓴다.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use windows_sys::Win32::Security::Authorization::SetSecurityInfo;
    use windows_sys::Win32::Security::PROTECTED_DACL_SECURITY_INFORMATION;
    use windows_sys::Win32::Security::{CreateWellKnownSid, WinBuiltinAdministratorsSid, WinWorldSid, SECURITY_MAX_SID_SIZE};
    use windows_sys::Win32::Storage::FileSystem::WRITE_DAC;

    pub(crate) const PRIVATE_DIR: Shape = Shape {
        reparse: false, directory: true, owner_is_user: true, dacl_present: true, dacl_protected: true, user_aces: 1, other_aces: 0,
    };
    pub(crate) const PRIVATE_FILE: Shape = Shape { directory: false, ..PRIVATE_DIR };

    fn well_known(kind: i32) -> Vec<u64> {
        let mut sid = vec![0u64; (SECURITY_MAX_SID_SIZE as usize).div_ceil(8)];
        let mut size = SECURITY_MAX_SID_SIZE;
        assert!(unsafe { CreateWellKnownSid(kind, null_mut(), sid.as_mut_ptr().cast(), &mut size) } != 0);
        sid
    }

    fn writable(path: &Path, access: u32) -> OwnedHandle {
        let name = wide(path).unwrap();
        owned(unsafe {
            CreateFileW(name.as_ptr(), READ_CONTROL | access, SHARE_ALL, null(), OPEN_EXISTING,
                FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS, null_mut())
        })
        .unwrap()
    }

    /// 사용자 ACE 옆에 Everyone 허용 ACE 를 단다(상속 차단은 그대로).
    pub(crate) fn add_everyone(path: &Path) {
        let user = User::current().unwrap();
        let world = well_known(WinWorldSid);
        let size = 2 * (std::mem::size_of::<ACCESS_ALLOWED_ACE>() + SECURITY_MAX_SID_SIZE as usize) + std::mem::size_of::<ACL>();
        let mut buffer = vec![0u64; size.div_ceil(8)];
        let acl = buffer.as_mut_ptr() as *mut ACL;
        unsafe {
            assert!(InitializeAcl(acl, size as u32, ACL_REVISION) != 0);
            assert!(AddAccessAllowedAceEx(acl, ACL_REVISION, 0, FILE_ALL_ACCESS, user.sid()) != 0);
            assert!(AddAccessAllowedAceEx(acl, ACL_REVISION, 0, FILE_ALL_ACCESS, world.as_ptr() as PSID) != 0);
            let code = SetSecurityInfo(raw(&writable(path, WRITE_DAC)), SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION, null_mut(), null_mut(), acl, null());
            assert_eq!(code, 0);
        }
    }

    /// NULL DACL — 문서대로 모두에게 열린다.
    pub(crate) fn null_dacl(path: &Path) {
        let code = unsafe {
            SetSecurityInfo(raw(&writable(path, WRITE_DAC)), SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION, null_mut(), null_mut(), null(), null())
        };
        assert_eq!(code, 0);
    }

    /// 소유자를 Administrators 그룹으로. 관리자 토큰이 아니면 OS 가 막는다 — 그때는 false.
    pub(crate) fn give_to_administrators(path: &Path) -> bool {
        use windows_sys::Win32::Storage::FileSystem::WRITE_OWNER;
        let admins = well_known(WinBuiltinAdministratorsSid);
        let name = wide(path).unwrap();
        let Ok(handle) = owned(unsafe {
            CreateFileW(name.as_ptr(), READ_CONTROL | WRITE_OWNER, SHARE_ALL, null(), OPEN_EXISTING,
                FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS, null_mut())
        }) else {
            return false;
        };
        unsafe {
            SetSecurityInfo(raw(&handle), SE_FILE_OBJECT, OWNER_SECURITY_INFORMATION, admins.as_ptr() as PSID,
                null_mut(), null(), null()) == 0
        }
    }

    /// 정션은 권한 없이 만들 수 있다 — 그래서 꼭 막아야 하는 바꿔치기다.
    pub(crate) fn junction(link: &Path, target: &Path) {
        let status = std::process::Command::new("cmd")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "mklink /J {} {}", link.display(), target.display());
    }

    pub(crate) fn scratch(name: &str) -> std::path::PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!("kasa-private-{name}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        base
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    #[test]
    fn created_directories_and_files_are_owner_only_from_the_start() {
        let base = scratch("create");
        let dir = base.join("store");
        let _guard = create_dir(&dir).unwrap();
        assert_eq!(describe(&dir).unwrap(), PRIVATE_DIR);
        assert_eq!(create_dir(&dir).err().map(|e| e.kind()), Some(io::ErrorKind::AlreadyExists));
        drop(create_new(&dir.join("a")).unwrap());
        assert_eq!(describe(&dir.join("a")).unwrap(), PRIVATE_FILE);
        assert_eq!(create_new(&dir.join("a")).err().map(|e| e.kind()), Some(io::ErrorKind::AlreadyExists));
        drop(open_existing(&dir.join("a"), true).unwrap());
        assert_eq!(open_existing(&dir.join("missing"), false).err().map(|e| e.kind()), Some(io::ErrorKind::NotFound));
        drop(open_or_create(&dir.join("lock")).unwrap());
        drop(open_or_create(&dir.join("lock")).unwrap());
        assert_eq!(describe(&dir.join("lock")).unwrap(), PRIVATE_FILE);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 상속 ACL 그대로인 폴더·Everyone 이 붙은 폴더·파일·NULL DACL 은 받지 않고, 고치지도 않는다.
    #[test]
    fn inherited_broad_or_null_acls_are_refused_and_left_as_they_are() {
        let base = scratch("broad");
        let inherited = base.join("inherited");
        std::fs::create_dir(&inherited).unwrap();
        let before = describe(&inherited).unwrap();
        assert!(!before.is_private(true), "{before:?}");
        assert!(verify_dir(&inherited).is_err());
        assert!(ensure_dir(&inherited).is_err());
        assert_eq!(describe(&inherited).unwrap(), before);

        let widened = base.join("widened");
        drop(create_dir(&widened).unwrap());
        drop(create_new(&widened.join("f")).unwrap());
        add_everyone(&widened.join("f"));
        assert!(open_existing(&widened.join("f"), false).is_err());
        assert!(open_or_create(&widened.join("f")).is_err());
        add_everyone(&widened);
        let before = describe(&widened).unwrap();
        assert_eq!(before.other_aces, 1);
        assert!(verify_dir(&widened).is_err());
        assert!(ensure_dir(&widened).is_err());
        assert_eq!(describe(&widened).unwrap(), before);

        let open = base.join("null");
        drop(create_dir(&open).unwrap());
        drop(create_new(&open.join("f")).unwrap());
        null_dacl(&open.join("f"));
        assert!(!describe(&open.join("f")).unwrap().dacl_present);
        assert!(open_existing(&open.join("f"), false).is_err());
        null_dacl(&open);
        assert!(!describe(&open).unwrap().dacl_present);
        assert!(verify_dir(&open).is_err());
        assert!(ensure_dir(&open).is_err());
        assert!(!describe(&open).unwrap().dacl_present);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 거부한 폴더의 기존 자식 — 상속 ACL 그대로인 것, 따로 DACL 을 둔 것, 남이 소유한 것 — 은 그대로 남는다.
    #[test]
    fn a_refused_directory_leaves_its_existing_children_untouched() {
        let base = scratch("children");
        let dir = base.join("shared");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("inherited.txt"), "x").unwrap();
        std::fs::write(dir.join("explicit.txt"), "x").unwrap();
        add_everyone(&dir.join("explicit.txt"));
        std::fs::write(dir.join("foreign.txt"), "x").unwrap();
        let foreign = give_to_administrators(&dir.join("foreign.txt"));
        let children = ["inherited.txt", "explicit.txt", "foreign.txt"];
        let before: Vec<Shape> = children.iter().map(|c| describe(&dir.join(c)).unwrap()).collect();
        if foreign {
            assert!(!before[2].owner_is_user);
        }
        assert!(ensure_dir(&dir).is_err());
        assert!(verify_dir(&dir).is_err());
        let after: Vec<Shape> = children.iter().map(|c| describe(&dir.join(c)).unwrap()).collect();
        assert_eq!(after, before);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn another_owner_is_refused_and_left_alone() {
        let base = scratch("owner");
        let dir = base.join("store");
        drop(create_dir(&dir).unwrap());
        if !give_to_administrators(&dir) {
            eprintln!("관리자 토큰이 아니라 소유자를 Administrators 로 못 바꾼다 — 이 경우는 관리자 러너에서 본다");
            return;
        }
        let shape = describe(&dir).unwrap();
        assert!(!shape.owner_is_user);
        assert!(verify_dir(&dir).is_err());
        assert!(ensure_dir(&dir).is_err());
        assert_eq!(describe(&dir).unwrap(), shape, "남의 디렉터리는 바로잡지 않는다");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn reparse_points_are_not_followed() {
        let base = scratch("reparse");
        let real = base.join("real");
        drop(create_dir(&real).unwrap());
        drop(create_new(&real.join("f")).unwrap());
        let link = base.join("link");
        junction(&link, &real);
        assert!(describe(&link).unwrap().reparse);
        assert!(verify_dir(&link).is_err());
        assert!(ensure_dir(&link).is_err());
        assert_eq!(describe(&real).unwrap(), PRIVATE_DIR, "정션 너머 진짜 폴더는 그대로");
        match std::os::windows::fs::symlink_file(real.join("f"), base.join("f-link")) {
            Ok(()) => {
                assert!(open_existing(&base.join("f-link"), false).is_err());
                assert!(open_or_create(&base.join("f-link")).is_err());
            }
            Err(e) => eprintln!("파일 심볼릭 링크를 만들 권한이 없다({e}) — 관리자·개발자 모드 러너에서 본다"),
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_held_directory_cannot_be_moved_away_but_its_files_can_change() {
        let base = scratch("held");
        let dir = base.join("store");
        let guard = create_dir(&dir).unwrap();
        assert!(std::fs::rename(&dir, base.join("moved")).is_err());
        drop(create_new(&dir.join("a.tmp")).unwrap());
        std::fs::rename(dir.join("a.tmp"), dir.join("a")).unwrap();
        std::fs::remove_file(dir.join("a")).unwrap();
        drop(guard);
        std::fs::rename(&dir, base.join("moved")).unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 경로 안의 NUL 은 Win32 가 거기서 끊어 앞부분을 연다 — 그 앞부분이 만들어지거나 열리면 안 된다.
    #[test]
    fn an_interior_nul_is_refused_before_anything_is_touched() {
        use std::os::windows::ffi::OsStringExt;
        let base = scratch("nul");
        let cut = base.join("store");
        let mut name: Vec<u16> = cut.as_os_str().encode_wide().collect();
        name.extend([0u16]);
        name.extend("evil".encode_utf16());
        let poisoned = std::path::PathBuf::from(std::ffi::OsString::from_wide(&name));
        let invalid = |r: io::Result<()>| assert_eq!(r.err().map(|e| e.kind()), Some(io::ErrorKind::InvalidInput));
        invalid(create_dir(&poisoned).map(drop));
        invalid(verify_dir(&poisoned).map(drop));
        invalid(ensure_dir(&poisoned).map(drop));
        invalid(create_new(&poisoned).map(drop));
        invalid(open_existing(&poisoned, false).map(drop));
        invalid(open_or_create(&poisoned).map(drop));
        invalid(describe(&poisoned).map(drop));
        assert!(!cut.exists(), "잘린 앞부분이 만들어졌다");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 장치·파이프 같은 디스크 밖 핸들은 저장소로 받지 않는다.
    #[test]
    fn device_handles_are_refused() {
        let nul = Path::new(r"\\.\NUL");
        assert!(open_existing(nul, false).is_err());
        assert!(open_or_create(nul).is_err());
        assert!(verify_dir(nul).is_err());
        assert!(describe(nul).is_err());
    }

    #[test]
    fn a_lock_file_holds_one_writer_until_it_is_closed() {
        let base = scratch("lock");
        let dir = base.join("store");
        let _guard = create_dir(&dir).unwrap();
        let first = open_or_create(&dir.join("lock")).unwrap();
        first.try_lock().unwrap();
        let second = open_or_create(&dir.join("lock")).unwrap();
        assert!(second.try_lock().is_err(), "같은 프로세스의 두 번째 핸들도 막힌다");
        drop(first);
        second.try_lock().unwrap();
        drop(second);
        let _ = std::fs::remove_dir_all(&base);
    }
}
