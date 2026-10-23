//! 루프백 Range 창구(`kasa_mcp::media_stream`)를 Media Foundation 바이트 스트림으로 읽는다. 재생 엔진의
//! 내장 HTTP 는 원본이 몇 초 멈추면 연결을 새로 맺으며 파일 처음부터 다시 받았고, 그동안 재생이 끊겨도
//! 알리지 않았다(느린 루프백 원본 실측) — 여기서는 엔진이 묻는 자리부터 `Range` 로 묻고, 끊기면 그
//! 자리에서 다시 묻는다. 엔진은 이 스트림 말고 다른 길로 원본에 닿지 않는다.

use std::collections::{HashMap, VecDeque};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use windows::core::{implement, Interface, Ref, BOOL, HRESULT};
use windows::Win32::Foundation::{E_INVALIDARG, E_NOTIMPL, E_POINTER, S_OK};
use windows::Win32::Media::MediaFoundation::{
    IMFAsyncCallback, IMFAsyncResult, IMFByteStream, IMFByteStream_Impl, MFCreateAsyncResult,
    MFInvokeCallback, MFBYTESTREAM_IS_READABLE, MFBYTESTREAM_IS_SEEKABLE,
    MFBYTESTREAM_SEEK_ORIGIN, MF_E_NET_READ, MF_E_SHUTDOWN, msoBegin, msoCurrent,
};

/// 열린 연결의 자리보다 이만큼 안쪽 앞이면 새로 묻지 않고 읽어 넘긴다 — 새 연결보다 싸다.
const SKIP: u64 = 256 << 10;
/// 원본이 잠깐 끊긴 것은 같은 자리에서 다시 묻는다. 그래도 안 되면 엔진에 읽기 실패를 알린다.
const RETRIES: u32 = 3;
const RETRY_WAIT: Duration = Duration::from_millis(500);
/// 정보 상자와 재생 구간을 번갈아 읽는 파일(끝에 moov, 소리·영상이 떨어져 놓인 것)도 매번 새로 묻지 않게.
const LINKS: usize = 2;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// 응답 머리 한도. 루프백 창구의 머리는 수백 바이트다.
const HEAD_LIMIT: usize = 16 << 10;
/// 엔진은 같은 자리를 거듭 읽는다(파일 머리를 16B·1KiB·256KiB 로 세 번). 최근 받은 조각을 이만큼 두고
/// 다시 읽기는 새 연결 없이 답한다.
const RECENT: usize = 2 << 20;

/// 등록소(`kasa_socket::inline_media`)가 내주는 형식 중 엔진이 읽는 것의 확장자와 MIME.
const FORMATS: &[(&str, &str)] = &[
    ("mp3", "audio/mpeg"),
    ("m4a", "audio/mp4"),
    ("m4b", "audio/mp4"),
    ("aac", "audio/aac"),
    ("wav", "audio/wav"),
    ("flac", "audio/flac"),
    ("mp4", "video/mp4"),
    ("m4v", "video/x-m4v"),
    ("mov", "video/quicktime"),
];

/// 한 미리보기의 원본. 엔진의 바이트 스트림과 재생기가 함께 쥐고, 재생기를 버리면 닫힌다.
pub(super) struct Source {
    addr: SocketAddr,
    host: String,
    path: String,
    links: Mutex<Vec<Link>>,
    /// 지금 기다리는 연결의 사본. 닫을 때 막힌 읽기를 바로 깨운다.
    active: Mutex<HashMap<u64, TcpStream>>,
    next_watch: std::sync::atomic::AtomicU64,
    length: Mutex<Option<u64>>,
    recent: Mutex<VecDeque<(u64, Vec<u8>)>>,
    closed: AtomicBool,
}

/// 열린 `bytes=at-` 응답 하나. `end` 는 본문이 끝나는 자리(포함하지 않음).
struct Link {
    reader: BufReader<TcpStream>,
    at: u64,
    end: u64,
}

struct Reply {
    status: u16,
    start: u64,
    end: u64,
    total: u64,
    mime: Option<String>,
}

impl Source {
    /// `http://127.0.0.1:<port>/…` 만 받는다(부르는 쪽이 이미 확인했다).
    pub(super) fn new(url: &str) -> Result<Arc<Self>, String> {
        let rest = url
            .strip_prefix("http://")
            .ok_or("media stream must be a loopback http URL")?;
        let slash = rest.find('/').ok_or("media stream URL is malformed")?;
        let (host, path) = rest.split_at(slash);
        let addr = host
            .parse()
            .map_err(|_| "media stream URL is malformed".to_string())?;
        Ok(Arc::new(Self {
            addr,
            host: host.to_string(),
            path: path.to_string(),
            links: Mutex::default(),
            active: Mutex::default(),
            next_watch: Default::default(),
            length: Mutex::default(),
            recent: Mutex::default(),
            closed: AtomicBool::new(false),
        }))
    }

    pub(super) fn close(&self) {
        self.closed.store(true, Ordering::Release);
        for (_, socket) in lock(&self.active).drain() {
            let _ = socket.shutdown(Shutdown::Both);
        }
        for link in lock(&self.links).drain(..) {
            let _ = link.reader.get_ref().shutdown(Shutdown::Both);
        }
    }

    /// 이 연결에서 기다리는 동안 닫기가 깨울 수 있게 걸어 둔다. 돌려준 값을 버리면 풀린다.
    fn watch(&self, socket: &TcpStream) -> io::Result<Watch<'_>> {
        let id = self.next_watch.fetch_add(1, Ordering::Relaxed);
        lock(&self.active).insert(id, socket.try_clone()?);
        // 걸기 전에 닫혔으면 깨워 줄 이가 없다.
        self.closed()?;
        Ok(Watch { source: self, id })
    }

    fn closed(&self) -> io::Result<()> {
        if self.closed.load(Ordering::Acquire) {
            Err(io::Error::new(io::ErrorKind::ConnectionAborted, "media stream closed"))
        } else {
            Ok(())
        }
    }

    /// 길이는 HEAD 로 묻는다 — 창구는 HEAD 에 원본을 기다리지 않고 답하고, GET 은 첫 조각을 받은 뒤에야 답한다.
    fn length(&self) -> io::Result<u64> {
        let known = *lock(&self.length);
        match known {
            Some(n) => Ok(n),
            None => self.head().map(|(total, _)| total),
        }
    }

    fn head(&self) -> io::Result<(u64, Option<String>)> {
        let (_, reply) = self.request("HEAD", None)?;
        if reply.status != 200 {
            return Err(http_error(reply.status));
        }
        *lock(&self.length) = Some(reply.total);
        Ok((reply.total, reply.mime))
    }

    /// 엔진이 형식을 고를 이름. 엔진은 이름의 확장자로 형식을 고르고, 모르면 판별기를 차례로 돌리며 파일을
    /// 끝까지 훑는다(실측) — 앱 창구 주소는 확장자를 싣고, 없으면 HEAD 의 Content-Type 으로 붙인다.
    pub(super) fn format_hint(&self, url: &str) -> String {
        let name = url.rsplit('/').next().unwrap_or_default();
        let known = |ext: &str| FORMATS.iter().any(|(e, _)| e.eq_ignore_ascii_case(ext));
        if name.rsplit_once('.').is_some_and(|(_, ext)| known(ext)) {
            return url.to_string();
        }
        let ext = self
            .head()
            .ok()
            .and_then(|(_, mime)| mime)
            .and_then(|mime| FORMATS.iter().find(|(_, m)| *m == mime).map(|(e, _)| *e));
        ext.map_or_else(|| url.to_string(), |ext| format!("media.{ext}"))
    }

    /// `offset` 부터 `buf` 를 채운다. 끝에 닿았을 때만 덜 채운다 — 엔진의 MP4 원본은 짧은 읽기를 끝으로
    /// 알아듣고 파일을 처음부터 다시 훑다가 실패했다(실측). 원본이 멈추면 여기서 기다린다.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        let total = self.length()?;
        if offset >= total || buf.is_empty() {
            return Ok(0);
        }
        let want = buf.len().min((total - offset) as usize);
        let mut filled = 0;
        while filled < want {
            let at = offset + filled as u64;
            let cached = self.from_recent(at, &mut buf[filled..want]);
            if cached > 0 {
                filled += cached;
                continue;
            }
            let n = self.read_network(at, &mut buf[filled..want])?;
            self.remember(at, &buf[filled..filled + n]);
            filled += n;
        }
        Ok(filled)
    }

    /// 한 번이라도 받을 때까지 연결을 바꿔 가며 묻는다. 0 은 돌려주지 않는다.
    fn read_network(&self, at: u64, dst: &mut [u8]) -> io::Result<usize> {
        let mut failures = 0;
        loop {
            self.closed()?;
            let (link, reused) = match self.take_link(at) {
                Some(link) => (Ok(link), true),
                None => (self.open(at), false),
            };
            let step = link.and_then(|mut link| {
                let n = self.fill(&mut link, at, dst)?;
                self.put_link(link);
                Ok(n)
            });
            match step {
                Ok(n) if n > 0 => return Ok(n),
                // 쉬는 동안 창구가 닫은 연결이다 — 같은 자리로 바로 다시 묻는다.
                Ok(_) | Err(_) if reused => continue,
                Ok(_) => {}
                Err(e) => {
                    self.closed()?;
                    if failures >= RETRIES {
                        return Err(e);
                    }
                }
            }
            failures += 1;
            if failures > RETRIES {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "media stream ended early"));
            }
            std::thread::sleep(RETRY_WAIT);
        }
    }

    /// 최근 받은 조각으로 `offset` 부터 이어지는 만큼 채운다.
    fn from_recent(&self, offset: u64, buf: &mut [u8]) -> usize {
        let recent = lock(&self.recent);
        let mut filled = 0;
        while filled < buf.len() {
            let at = offset + filled as u64;
            let Some((start, bytes)) = recent
                .iter()
                .find(|(start, bytes)| *start <= at && at < start + bytes.len() as u64)
            else {
                break;
            };
            let from = (at - start) as usize;
            let n = (bytes.len() - from).min(buf.len() - filled);
            buf[filled..filled + n].copy_from_slice(&bytes[from..from + n]);
            filled += n;
        }
        filled
    }

    fn remember(&self, offset: u64, bytes: &[u8]) {
        if bytes.is_empty() || bytes.len() > RECENT {
            return;
        }
        let mut recent = lock(&self.recent);
        recent.push_back((offset, bytes.to_vec()));
        let mut kept: usize = recent.iter().map(|(_, b)| b.len()).sum();
        while kept > RECENT {
            match recent.pop_front() {
                Some((_, b)) => kept -= b.len(),
                None => break,
            }
        }
    }

    fn take_link(&self, at: u64) -> Option<Link> {
        let mut links = lock(&self.links);
        let index = links
            .iter()
            .position(|l| l.at <= at && at - l.at <= SKIP && at < l.end)?;
        Some(links.remove(index))
    }

    fn put_link(&self, link: Link) {
        if link.at >= link.end || self.closed.load(Ordering::Acquire) {
            return;
        }
        let mut links = lock(&self.links);
        links.push(link);
        if links.len() > LINKS {
            links.remove(0);
        }
    }

    fn open(&self, at: u64) -> io::Result<Link> {
        let (reader, reply) = self.request("GET", Some(at))?;
        let fits = match reply.status {
            206 => reply.start == at,
            200 => at == 0,
            status => return Err(http_error(status)),
        };
        if !fits {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "media stream answered another range"));
        }
        lock(&self.length).get_or_insert(reply.total);
        Ok(Link {
            reader,
            at,
            end: reply.end,
        })
    }

    /// 연결의 자리를 `at` 까지 넘긴 뒤 `dst` 를 채운다. 연결이 먼저 끝나거나 끊기면 받은 만큼만.
    fn fill(&self, link: &mut Link, at: u64, dst: &mut [u8]) -> io::Result<usize> {
        let _watch = self.watch(link.reader.get_ref())?;
        let gap = at - link.at;
        if gap > 0 {
            let skipped = io::copy(&mut (&mut link.reader).take(gap), &mut io::sink())?;
            link.at += skipped;
            if skipped < gap {
                link.end = link.at;
                return Ok(0);
            }
        }
        let want = dst.len().min((link.end - link.at) as usize);
        let mut filled = 0;
        while filled < want {
            match link.reader.read(&mut dst[filled..want]) {
                Ok(0) => {
                    link.end = link.at;
                    break;
                }
                Ok(n) => {
                    filled += n;
                    link.at += n as u64;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                // 받은 만큼은 내주고 끊긴 연결은 버린다 — 나머지는 같은 자리에서 새로 묻는다.
                Err(_) if filled > 0 => {
                    link.end = link.at;
                    break;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(filled)
    }

    fn request(&self, method: &str, offset: Option<u64>) -> io::Result<(BufReader<TcpStream>, Reply)> {
        self.closed()?;
        let socket = TcpStream::connect_timeout(&self.addr, CONNECT_TIMEOUT)?;
        let _ = socket.set_nodelay(true);
        let range = offset.map_or(String::new(), |at| format!("Range: bytes={at}-\r\n"));
        // 창구는 Host 가 자기 주소와 같아야 받는다(DNS 리바인딩 막기).
        let head = format!(
            "{method} {} HTTP/1.1\r\nHost: {}\r\n{range}Accept-Encoding: identity\r\n\r\n",
            self.path, self.host
        );
        (&socket).write_all(head.as_bytes())?;
        let _watch = self.watch(&socket)?;
        read_reply(BufReader::new(socket))
    }
}

struct Watch<'a> {
    source: &'a Source,
    id: u64,
}

impl Drop for Watch<'_> {
    fn drop(&mut self) {
        lock(&self.source.active).remove(&self.id);
    }
}

fn read_reply(mut reader: BufReader<TcpStream>) -> io::Result<(BufReader<TcpStream>, Reply)> {
    let bad = || io::Error::new(io::ErrorKind::InvalidData, "media stream sent a malformed reply");
    let mut line = String::new();
    let mut seen = 0;
    reader.read_line(&mut line)?;
    seen += line.len();
    let status: u16 = line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(bad)?;
    let (mut length, mut range, mut mime) = (None, None, None);
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Err(bad());
        }
        seen += line.len();
        if seen > HEAD_LIMIT {
            return Err(bad());
        }
        let text = line.trim_end();
        if text.is_empty() {
            break;
        }
        let Some((name, value)) = text.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            length = value.parse::<u64>().ok();
        } else if name.eq_ignore_ascii_case("content-range") {
            range = Some(value.to_string());
        } else if name.eq_ignore_ascii_case("content-type") {
            mime = value.split(';').next().map(|m| m.trim().to_ascii_lowercase());
        } else if name.eq_ignore_ascii_case("transfer-encoding") && !value.eq_ignore_ascii_case("identity") {
            return Err(bad());
        }
    }
    let reply = match status {
        206 => {
            let (start, end, total) = range.as_deref().and_then(content_range).ok_or_else(bad)?;
            if length.is_some_and(|n| n != end + 1 - start) {
                return Err(bad());
            }
            Reply {
                status,
                start,
                end: end + 1,
                total,
                mime,
            }
        }
        200 => {
            let total = length.ok_or_else(bad)?;
            Reply {
                status,
                start: 0,
                end: total,
                total,
                mime,
            }
        }
        _ => Reply {
            status,
            start: 0,
            end: 0,
            total: 0,
            mime,
        },
    };
    Ok((reader, reply))
}

/// `bytes a-b/total` → `(a, b, total)`.
fn content_range(value: &str) -> Option<(u64, u64, u64)> {
    let spec = value.get(..6).filter(|u| u.eq_ignore_ascii_case("bytes "))?;
    let rest = &value[spec.len()..];
    let (span, total) = rest.split_once('/')?;
    let (a, b) = span.split_once('-')?;
    let (a, b, total) = (a.trim().parse().ok()?, b.trim().parse().ok()?, total.trim().parse().ok()?);
    (a <= b && b < total).then_some((a, b, total))
}

fn http_error(status: u16) -> io::Error {
    io::Error::other(format!("media stream answered HTTP {status}"))
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn hresult(e: &io::Error) -> HRESULT {
    if e.kind() == io::ErrorKind::ConnectionAborted {
        MF_E_SHUTDOWN
    } else {
        MF_E_NET_READ
    }
}

/// 엔진의 비동기 읽기 한 건. 버퍼는 엔진이 `EndRead` 까지 살려 둔다.
struct Job {
    offset: u64,
    /// 건 때의 `Cursor::moves`. 끝나기 전에 엔진이 자리를 옮겼으면 읽은 만큼 자리를 옮기지 않는다.
    moves: u64,
    buf: *mut u8,
    len: usize,
    result: IMFAsyncResult,
}

// 버퍼 포인터는 엔진이 완료 콜백까지 붙들고, 비동기 결과 객체는 Media Foundation 의 자유 스레드 객체다.
unsafe impl Send for Job {}

/// 읽기 자리. 비동기 읽기는 실제로 읽은 만큼만 자리를 옮긴다 — 원본이 느리면 요청보다 덜 읽는다.
#[derive(Default)]
struct Cursor {
    at: u64,
    moves: u64,
}

impl Cursor {
    /// 엔진은 읽기를 건 직후 `Seek(현재, 0)` 으로 자리를 묻기도 한다 — 제자리 이동은 옮긴 것으로 치지 않는다.
    fn move_to(&mut self, at: u64) {
        if at != self.at {
            self.at = at;
            self.moves += 1;
        }
    }
}

#[implement(IMFByteStream)]
pub(super) struct ByteStream {
    source: Arc<Source>,
    position: Arc<Mutex<Cursor>>,
    jobs: Mutex<Option<mpsc::Sender<Job>>>,
    /// 끝난 비동기 읽기의 바이트 수. 결과 객체 주소로 찾는다.
    done: Arc<Mutex<HashMap<usize, u32>>>,
}

impl ByteStream {
    pub(super) fn new(source: Arc<Source>) -> Result<IMFByteStream, String> {
        let (tx, rx) = mpsc::channel::<Job>();
        let done: Arc<Mutex<HashMap<usize, u32>>> = Arc::default();
        let position: Arc<Mutex<Cursor>> = Arc::default();
        let (reader, finished, cursor) = (Arc::clone(&source), Arc::clone(&done), Arc::clone(&position));
        // 엔진의 작업 큐를 원본 기다림으로 막지 않으려고 읽기는 따로 돈다.
        std::thread::Builder::new()
            .name("media-stream-read".into())
            .spawn(move || {
                for job in rx {
                    let buf = unsafe { std::slice::from_raw_parts_mut(job.buf, job.len) };
                    let (status, n) = match reader.read_at(job.offset, buf) {
                        Ok(n) => (S_OK, n as u32),
                        Err(e) => (hresult(&e), 0),
                    };
                    {
                        let mut cursor = lock(&cursor);
                        if cursor.moves == job.moves {
                            cursor.at = job.offset + n as u64;
                        }
                    }
                    lock(&finished).insert(job.result.as_raw() as usize, n);
                    unsafe {
                        let _ = job.result.SetStatus(status);
                        let _ = MFInvokeCallback(&job.result);
                    }
                }
            })
            .map_err(|e| format!("media player failed: {e}"))?;
        Ok(Self {
            source,
            position,
            jobs: Mutex::new(Some(tx)),
            done,
        }
        .into())
    }
}

impl Drop for ByteStream {
    fn drop(&mut self) {
        self.source.close();
    }
}

impl IMFByteStream_Impl for ByteStream_Impl {
    fn GetCapabilities(&self) -> windows::core::Result<u32> {
        Ok(MFBYTESTREAM_IS_READABLE | MFBYTESTREAM_IS_SEEKABLE)
    }

    fn GetLength(&self) -> windows::core::Result<u64> {
        self.source.length().map_err(|e| hresult(&e).into())
    }

    fn SetLength(&self, _: u64) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn GetCurrentPosition(&self) -> windows::core::Result<u64> {
        Ok(lock(&self.position).at)
    }

    fn SetCurrentPosition(&self, position: u64) -> windows::core::Result<()> {
        lock(&self.position).move_to(position);
        Ok(())
    }

    fn IsEndOfStream(&self) -> windows::core::Result<BOOL> {
        let length = self.source.length().map_err(|e| hresult(&e))?;
        Ok((lock(&self.position).at >= length).into())
    }

    fn Read(&self, pb: *mut u8, cb: u32, pcbread: *mut u32) -> windows::core::Result<()> {
        if pb.is_null() || pcbread.is_null() {
            return Err(E_POINTER.into());
        }
        let (offset, moves) = {
            let cursor = lock(&self.position);
            (cursor.at, cursor.moves)
        };
        let buf = unsafe { std::slice::from_raw_parts_mut(pb, cb as usize) };
        let n = self.source.read_at(offset, buf).map_err(|e| hresult(&e))?;
        let mut cursor = lock(&self.position);
        if cursor.moves == moves {
            cursor.at = offset + n as u64;
        }
        unsafe { *pcbread = n as u32 };
        Ok(())
    }

    fn BeginRead(
        &self,
        pb: *mut u8,
        cb: u32,
        pcallback: Ref<'_, IMFAsyncCallback>,
        punkstate: Ref<'_, windows::core::IUnknown>,
    ) -> windows::core::Result<()> {
        if pb.is_null() {
            return Err(E_POINTER.into());
        }
        let callback = pcallback.ok()?;
        let result = unsafe { MFCreateAsyncResult(None, callback, punkstate.as_ref())? };
        let (offset, moves) = {
            let cursor = lock(&self.position);
            (cursor.at, cursor.moves)
        };
        let jobs = lock(&self.jobs);
        let sender = jobs.as_ref().ok_or(windows::core::Error::from(MF_E_SHUTDOWN))?;
        sender
            .send(Job {
                offset,
                moves,
                buf: pb,
                len: cb as usize,
                result,
            })
            .map_err(|_| MF_E_SHUTDOWN.into())
    }

    fn EndRead(&self, presult: Ref<'_, IMFAsyncResult>) -> windows::core::Result<u32> {
        let result = presult.ok()?;
        let n = lock(&self.done).remove(&(result.as_raw() as usize)).unwrap_or(0);
        unsafe { result.GetStatus()? };
        Ok(n)
    }

    fn Write(&self, _: *const u8, _: u32) -> windows::core::Result<u32> {
        Err(E_NOTIMPL.into())
    }

    fn BeginWrite(
        &self,
        _: *const u8,
        _: u32,
        _: Ref<'_, IMFAsyncCallback>,
        _: Ref<'_, windows::core::IUnknown>,
    ) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn EndWrite(&self, _: Ref<'_, IMFAsyncResult>) -> windows::core::Result<u32> {
        Err(E_NOTIMPL.into())
    }

    fn Seek(
        &self,
        origin: MFBYTESTREAM_SEEK_ORIGIN,
        offset: i64,
        _: u32,
    ) -> windows::core::Result<u64> {
        let mut cursor = lock(&self.position);
        let base = match origin {
            o if o == msoBegin => 0i64,
            o if o == msoCurrent => cursor.at as i64,
            _ => return Err(E_INVALIDARG.into()),
        };
        let next = base
            .checked_add(offset)
            .filter(|p| *p >= 0)
            .ok_or(windows::core::Error::from(E_INVALIDARG))?;
        cursor.move_to(next as u64);
        Ok(cursor.at)
    }

    fn Flush(&self) -> windows::core::Result<()> {
        Ok(())
    }

    fn Close(&self) -> windows::core::Result<()> {
        lock(&self.jobs).take();
        self.source.close();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::content_range;

    #[test]
    fn content_range_reads_the_single_span_form() {
        assert_eq!(content_range("bytes 0-9/10"), Some((0, 9, 10)));
        assert_eq!(content_range("Bytes 4-4/5"), Some((4, 4, 5)));
        assert_eq!(content_range("bytes */10"), None);
        assert_eq!(content_range("bytes 5-2/10"), None);
        assert_eq!(content_range("bytes 0-10/10"), None);
        assert_eq!(content_range("items 0-1/2"), None);
    }
}
