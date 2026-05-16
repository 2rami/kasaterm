//! Cross-platform local socket abstraction.
//!
//! Unix: thin re-export of `std::os::unix::net::{UnixListener, UnixStream}`.
//! Windows: hand-rolled named-pipe wrapper exposing the same surface
//! (bind / accept / connect / Read / Write / try_clone).
//!
//! The protocol layer above only needs:
//!   - `LocalListener::bind(path)`
//!   - `listener.incoming()` yielding `LocalStream`s
//!   - `LocalStream::connect(path)` for the client side
//!   - `LocalStream: Read + Write + try_clone(&self) -> io::Result<Self>`
//!
//! Path semantics differ:
//!   - On Unix the `path` is taken verbatim as a filesystem path.
//!   - On Windows we accept any string; if it does not already start
//!     with `\\.\pipe\` we treat the final path component as a pipe
//!     name and prepend the prefix. Callers can pass a filesystem-style
//!     name on both platforms and get sensible behavior.

use std::path::Path;

#[cfg(unix)]
mod imp {
    use std::io::Result;
    use std::os::unix::net::{Incoming as UnixIncoming, UnixListener, UnixStream};
    use std::path::{Path, PathBuf};

    pub struct LocalListener {
        inner: UnixListener,
        path: PathBuf,
    }

    pub struct LocalStream {
        inner: UnixStream,
    }

    pub struct Incoming<'a> {
        inner: UnixIncoming<'a>,
    }

    impl LocalListener {
        pub fn bind(path: &Path) -> Result<Self> {
            let inner = UnixListener::bind(path)?;
            Ok(Self {
                inner,
                path: path.to_path_buf(),
            })
        }

        pub fn incoming(&self) -> Incoming<'_> {
            Incoming {
                inner: self.inner.incoming(),
            }
        }

        pub fn path(&self) -> &Path {
            &self.path
        }
    }

    impl<'a> Iterator for Incoming<'a> {
        type Item = Result<LocalStream>;
        fn next(&mut self) -> Option<Self::Item> {
            self.inner
                .next()
                .map(|r| r.map(|inner| LocalStream { inner }))
        }
    }

    impl LocalStream {
        pub fn connect(path: &Path) -> Result<Self> {
            let inner = UnixStream::connect(path)?;
            Ok(Self { inner })
        }

        pub fn try_clone(&self) -> Result<Self> {
            Ok(Self {
                inner: self.inner.try_clone()?,
            })
        }
    }

    impl std::io::Read for LocalStream {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
            self.inner.read(buf)
        }
    }

    impl std::io::Write for LocalStream {
        fn write(&mut self, buf: &[u8]) -> Result<usize> {
            self.inner.write(buf)
        }
        fn flush(&mut self) -> Result<()> {
            self.inner.flush()
        }
    }

    pub fn resolve_path(p: &Path) -> PathBuf {
        p.to_path_buf()
    }
}

#[cfg(windows)]
mod imp {
    use std::ffi::OsStr;
    use std::io::{self, Read, Result, Write};
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use std::ptr;
    use std::sync::Mutex;
    use windows_sys::Win32::Foundation::{
        CloseHandle, DuplicateHandle, DUPLICATE_SAME_ACCESS, ERROR_PIPE_CONNECTED,
        GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, ReadFile, WriteFile, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
    };
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
        PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    // RAII wrapper around a raw HANDLE so reads/writes from cloned
    // streams don't double-close the underlying pipe.
    struct OwnedHandle(HANDLE);

    impl OwnedHandle {
        fn new(h: HANDLE) -> Self {
            Self(h)
        }
        fn raw(&self) -> HANDLE {
            self.0
        }
    }

    // HANDLE is just a void pointer wrapping a kernel ref; the kernel
    // serialises access. We need this so Arc<OwnedHandle> can cross
    // thread boundaries when handle_client moves the stream into a
    // worker thread.
    unsafe impl Send for OwnedHandle {}
    unsafe impl Sync for OwnedHandle {}

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }
    }

    pub struct LocalListener {
        // The full \\.\pipe\name string, kept so each accept() can
        // create a fresh instance (Windows named pipes serve one client
        // per instance; we re-create on every connection).
        pipe_name_wide: Vec<u16>,
        display_path: PathBuf,
        // Currently-armed pipe instance waiting in ConnectNamedPipe.
        // None between accept() calls; Some() once arm_next_instance
        // has placed a pipe in the listen state.
        current: Mutex<Option<OwnedHandle>>,
    }

    pub struct LocalStream {
        // Mutex<HANDLE> rather than OwnedHandle directly so &mut self
        // is enough for Read/Write but try_clone() can share ownership.
        handle: std::sync::Arc<OwnedHandle>,
    }

    pub struct Incoming<'a> {
        listener: &'a LocalListener,
    }

    fn pipe_name_to_wide(p: &Path) -> Vec<u16> {
        let s = p.to_string_lossy().into_owned();
        let s = if s.starts_with(r"\\.\pipe\") {
            s
        } else {
            // Reduce a filesystem-style path down to its final segment
            // and graft it onto the namespace prefix. Slashes, dots and
            // .sock suffixes that happen to be in the original path are
            // legal pipe-name characters, so we keep them as-is.
            let base = p
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| s.clone());
            format!(r"\\.\pipe\{base}")
        };
        OsStr::new(&s)
            .encode_wide()
            .chain(std::iter::once(0u16))
            .collect()
    }

    fn create_pipe_instance(name_wide: &[u16]) -> Result<OwnedHandle> {
        let h = unsafe {
            CreateNamedPipeW(
                name_wide.as_ptr(),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                64 * 1024,
                64 * 1024,
                0,
                ptr::null(),
            )
        };
        if h.is_null() || h == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(OwnedHandle::new(h))
    }

    impl LocalListener {
        pub fn bind(path: &Path) -> Result<Self> {
            let pipe_name_wide = pipe_name_to_wide(path);
            // Create the first instance up-front so a CLIENT can connect
            // before we ever call accept(). Without this, the pipe name
            // would not exist between bind() and the first accept().
            let first = create_pipe_instance(&pipe_name_wide)?;
            Ok(Self {
                pipe_name_wide,
                display_path: path.to_path_buf(),
                current: Mutex::new(Some(first)),
            })
        }

        pub fn incoming(&self) -> Incoming<'_> {
            Incoming { listener: self }
        }

        pub fn path(&self) -> &Path {
            &self.display_path
        }

        // Block until a client connects, hand it the currently-armed
        // pipe instance, then arm a new instance for the next accept.
        fn accept_one(&self) -> Result<LocalStream> {
            // Take ownership of the armed instance; we'll consume it.
            let owned = {
                let mut g = self.current.lock().expect("listener mutex poisoned");
                match g.take() {
                    Some(h) => h,
                    None => create_pipe_instance(&self.pipe_name_wide)?,
                }
            };
            let raw = owned.raw();
            let connected = unsafe { ConnectNamedPipe(raw, ptr::null_mut()) };
            // ConnectNamedPipe returns nonzero on success. If it returns
            // 0, ERROR_PIPE_CONNECTED means the client beat us to it —
            // not an error.
            if connected == 0 {
                let err = io::Error::last_os_error();
                if err.raw_os_error() != Some(ERROR_PIPE_CONNECTED as i32) {
                    return Err(err);
                }
            }
            // Arm the next instance so a fresh accept() can run while
            // this connection is being serviced on a worker thread.
            let next = create_pipe_instance(&self.pipe_name_wide)?;
            *self.current.lock().expect("listener mutex poisoned") = Some(next);
            Ok(LocalStream {
                handle: std::sync::Arc::new(owned),
            })
        }
    }

    impl Drop for LocalListener {
        fn drop(&mut self) {
            if let Some(h) = self.current.lock().ok().and_then(|mut g| g.take()) {
                unsafe {
                    DisconnectNamedPipe(h.raw());
                }
            }
        }
    }

    impl<'a> Iterator for Incoming<'a> {
        type Item = Result<LocalStream>;
        fn next(&mut self) -> Option<Self::Item> {
            Some(self.listener.accept_one())
        }
    }

    impl LocalStream {
        pub fn connect(path: &Path) -> Result<Self> {
            let name_wide = pipe_name_to_wide(path);
            let h = unsafe {
                CreateFileW(
                    name_wide.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    0,
                    ptr::null(),
                    OPEN_EXISTING,
                    0,
                    ptr::null_mut(),
                )
            };
            if h.is_null() || h == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            Ok(Self {
                handle: std::sync::Arc::new(OwnedHandle::new(h)),
            })
        }

        pub fn try_clone(&self) -> Result<Self> {
            // Duplicate the HANDLE so each clone owns an independent
            // refcount the kernel will close. Arc-sharing the OwnedHandle
            // would work for read+write split too, but DuplicateHandle
            // matches the UnixStream::try_clone semantics more closely
            // (close one side, the other keeps working).
            let mut dup: HANDLE = ptr::null_mut();
            let ok = unsafe {
                DuplicateHandle(
                    GetCurrentProcess(),
                    self.handle.raw(),
                    GetCurrentProcess(),
                    &mut dup,
                    0,
                    0,
                    DUPLICATE_SAME_ACCESS,
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self {
                handle: std::sync::Arc::new(OwnedHandle::new(dup)),
            })
        }
    }

    impl Read for LocalStream {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
            let mut got: u32 = 0;
            let len = buf.len().min(u32::MAX as usize) as u32;
            let ok = unsafe {
                ReadFile(
                    self.handle.raw(),
                    buf.as_mut_ptr() as *mut _,
                    len,
                    &mut got,
                    ptr::null_mut(),
                )
            };
            if ok == 0 {
                let err = io::Error::last_os_error();
                // BrokenPipe on a closed peer is the canonical "EOF"
                // for byte-stream sockets. UnixStream surfaces this as
                // Ok(0); mirror that so framing readers (BufRead::lines)
                // terminate cleanly instead of returning an error.
                if matches!(
                    err.raw_os_error().map(|c| c as u32),
                    Some(windows_sys::Win32::Foundation::ERROR_BROKEN_PIPE)
                ) {
                    return Ok(0);
                }
                return Err(err);
            }
            Ok(got as usize)
        }
    }

    impl Write for LocalStream {
        fn write(&mut self, buf: &[u8]) -> Result<usize> {
            let mut wrote: u32 = 0;
            let len = buf.len().min(u32::MAX as usize) as u32;
            let ok = unsafe {
                WriteFile(
                    self.handle.raw(),
                    buf.as_ptr() as *const _,
                    len,
                    &mut wrote,
                    ptr::null_mut(),
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(wrote as usize)
        }
        fn flush(&mut self) -> Result<()> {
            // Windows named pipes are not user-space buffered on our
            // side — WriteFile already hands bytes to the kernel.
            Ok(())
        }
    }

    pub fn resolve_path(p: &Path) -> PathBuf {
        // Reflect what bind/connect will actually use, for diagnostics.
        let s = p.to_string_lossy();
        if s.starts_with(r"\\.\pipe\") {
            p.to_path_buf()
        } else {
            let base = p
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| s.into_owned());
            PathBuf::from(format!(r"\\.\pipe\{base}"))
        }
    }
}

pub use imp::{Incoming, LocalListener, LocalStream};

pub fn resolved_path(p: &Path) -> std::path::PathBuf {
    imp::resolve_path(p)
}
